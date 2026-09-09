use super::tests::{test_service_with_llm, test_service_with_manager};
use super::*;
use crate::session::{TurnDispatch, TurnRequest};
use std::time::Duration;

struct Done;
#[async_trait::async_trait]
impl llm::LLMProvider for Done {
    async fn send_message(
        &mut self,
        _: llm::LLMRequest,
        _: Option<&llm::StreamingCallback>,
    ) -> Result<llm::LLMResponse> {
        Ok(llm::LLMResponse {
            content: vec![llm::ContentBlock::new_text("done")],
            usage: llm::Usage::zero(),
            rate_limit_info: None,
        })
    }
}

fn blocked_registry(
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
) -> crate::session::manager::ToolRegistryProvider {
    Arc::new(move |_| {
        let entered = entered.clone();
        let release = release.clone();
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            crate::tools::test_registry()
        })
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_pending_setup_does_not_keep_its_owner_alive() {
    let tmp = tempfile::tempdir().unwrap();
    let (service, manager) = test_service_with_llm(tmp.path(), Arc::new(|_| Ok(Box::new(Done))));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    manager
        .lock()
        .await
        .set_tool_registry_provider(blocked_registry(entered.clone(), release));
    let id = service.create_session(None, None).await.unwrap();
    let TurnDispatch::Started(handle) = service
        .start_turn_if_idle(id.clone(), TurnRequest::text("task"))
        .await
        .unwrap()
    else {
        panic!("fresh session busy")
    };
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    let inhibitor = manager.lock().await.sleep_inhibitor();
    assert_eq!(inhibitor.running_count(), 1);
    let weak = Arc::downgrade(&manager);
    drop(manager);
    drop(service);
    let released = tokio::time::timeout(Duration::from_millis(250), async {
        while weak.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    // Cleanup even on RED: do not leave a pending task or file lock behind.
    if let Some(manager) = weak.upgrade() {
        manager.lock().await.terminate_session_agent(&id);
    }
    assert!(
        released.is_ok(),
        "setup retained its owner in a reference cycle"
    );
    tokio::time::timeout(Duration::from_secs(2), handle.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        inhibitor.running_count(),
        0,
        "shutdown leaked the run's wake lock"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_setup_preserves_a_newer_model_selection() {
    let tmp = tempfile::tempdir().unwrap();
    let (service, manager) = test_service_with_llm(tmp.path(), Arc::new(|_| Ok(Box::new(Done))));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    manager
        .lock()
        .await
        .set_tool_registry_provider(blocked_registry(entered.clone(), release.clone()));
    let id = service.create_session(None, None).await.unwrap();
    let TurnDispatch::Started(handle) = service
        .start_turn_if_idle(id.clone(), TurnRequest::text("task"))
        .await
        .unwrap()
    else {
        panic!("fresh session busy")
    };
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    // Simulate a selection arriving during preparation without depending on
    // the developer's models.json or any real provider configuration.
    let next_model = SessionModelConfig::new("selected-during-setup".into());
    {
        let mut manager = manager.lock().await;
        manager.get_session_mut(&id).unwrap().session.model_config = Some(next_model.clone());
        let mut store =
            crate::persistence::FileSessionPersistence::new_with_root_dir(tmp.path().to_path_buf());
        store
            .update_entry(&id, |session| {
                session.model_config = Some(next_model);
                Ok(())
            })
            .unwrap();
    }
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), handle.wait())
        .await
        .unwrap()
        .unwrap();
    let snapshot = service.load_session(id, None).await.unwrap();
    assert_eq!(snapshot.current_model, "selected-during-setup");
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_external_run_rejection_does_not_append_a_message() {
    let tmp = tempfile::tempdir().unwrap();
    let (service, _) = test_service_with_manager(tmp.path());
    let id = service.create_session(None, None).await.unwrap();
    let lock = crate::utils::file_utils::try_acquire_agent_lock(&tmp.path().join("sessions"), &id)
        .unwrap()
        .unwrap();
    let sent = service
        .send_user_message(id.clone(), "must not land".into(), vec![], None)
        .await;
    drop(lock);
    assert!(sent.is_err());
    let snapshot = service.load_session(id, None).await.unwrap();
    assert!(
        snapshot.messages.is_empty(),
        "a refused turn modified the conversation"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_stop_during_permission_does_not_open_the_next_prompt() {
    struct TwoCalls;
    #[async_trait::async_trait]
    impl llm::LLMProvider for TwoCalls {
        async fn send_message(
            &mut self,
            _: llm::LLMRequest,
            _: Option<&llm::StreamingCallback>,
        ) -> Result<llm::LLMResponse> {
            Ok(llm::LLMResponse {
                content: ["first", "second"]
                    .into_iter()
                    .map(|id| {
                        llm::ContentBlock::new_tool_use(
                            id,
                            "read_files",
                            serde_json::json!({"project":"test", "paths":["a.rs"]}),
                        )
                    })
                    .collect(),
                usage: llm::Usage::zero(),
                rate_limit_info: None,
            })
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let (service, manager) =
        test_service_with_llm(tmp.path(), Arc::new(|_| Ok(Box::new(TwoCalls))));
    let id = service.create_session(None, None).await.unwrap();
    service
        .change_permission_tier(id.clone(), tools_core::PermissionTier::AllTools)
        .await
        .unwrap();
    let mut events = service.subscribe();
    let TurnDispatch::Started(handle) = service
        .start_turn_if_idle(id.clone(), TurnRequest::text("read"))
        .await
        .unwrap()
    else {
        panic!("busy")
    };
    let first = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let crate::session::EventPayload::Ui(UiEvent::RequestToolPermission { request }) =
                events.recv().await.unwrap().payload
            {
                break request;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(first.tool_id.as_deref(), Some("first"));
    service.request_stop(id.clone()).await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(2), handle.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.status, crate::session::TurnStatus::Cancelled);
    assert!(
        manager
            .lock()
            .await
            .get_session(&id)
            .unwrap()
            .pending_permission_requests
            .snapshot()
            .is_empty()
    );
    // An event after completion fences every earlier prompt publication.
    service.clear_session_error(id).await.unwrap();
    loop {
        match events.recv().await.unwrap().payload {
            crate::session::EventPayload::Ui(UiEvent::RequestToolPermission { .. }) => {
                panic!("a second prompt opened after stop")
            }
            crate::session::EventPayload::Ui(UiEvent::UpdateSessionActivityState {
                activity_state,
                ..
            }) if activity_state.is_terminal() => break,
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_stop_wakes_an_already_waiting_silent_provider() {
    struct Silent(Arc<tokio::sync::Notify>);
    #[async_trait::async_trait]
    impl llm::LLMProvider for Silent {
        async fn send_message(
            &mut self,
            _: llm::LLMRequest,
            _: Option<&llm::StreamingCallback>,
        ) -> Result<llm::LLMResponse> {
            self.0.notify_one();
            std::future::pending().await
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let (service, _) = test_service_with_llm(tmp.path(), {
        let entered = entered.clone();
        Arc::new(move |_| Ok(Box::new(Silent(entered.clone()))))
    });
    let id = service.create_session(None, None).await.unwrap();
    let TurnDispatch::Started(handle) = service
        .start_turn_if_idle(id.clone(), TurnRequest::text("wait"))
        .await
        .unwrap()
    else {
        panic!("busy")
    };
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    handle.cancel().await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(2), handle.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.status, crate::session::TurnStatus::Cancelled);
    assert!(!service.is_session_busy(id).await.unwrap());
}

#[tokio::test]
async fn recovery_slow_io_does_not_block_session_control() {
    let tmp = tempfile::tempdir().unwrap();
    let (service, manager) = test_service_with_manager(tmp.path());
    let id = service.create_session(None, None).await.unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let (release, released) = std::sync::mpsc::channel();
    let task = tokio::spawn({
        let service = service.clone();
        let entered = entered.clone();
        async move {
            service
                .call_io(move |_| async move {
                    entered.notify_one();
                    released.recv_timeout(Duration::from_secs(3)).unwrap();
                    Ok(())
                })
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    let stopped =
        tokio::time::timeout(Duration::from_millis(250), service.request_stop(id.clone())).await;
    release.send(()).unwrap();
    task.await.unwrap().unwrap();
    stopped.expect("slow IO blocked stop").unwrap();
    assert!(
        manager
            .lock()
            .await
            .get_session(&id)
            .unwrap()
            .cancellation
            .is_cancelled()
    );
}
