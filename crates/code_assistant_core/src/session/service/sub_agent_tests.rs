//! Background sub-agents end to end: the parent's turn ends while the child
//! runs, and the child's result comes back into the session as a message.

use super::tests::test_service_with_manager;
use super::*;
use crate::agent::sub_agent::{
    FinishedOutcome, FinishedSubAgent, SubAgentActivity, SubAgentOutput,
};
use crate::mocks::{MockLLMProvider, create_test_response, create_test_response_text};
use crate::persistence::FileSessionPersistence;
use crate::session::SessionConfig;
use crate::tools::impls::spawn_agent::SpawnAgentOutput;
use llm::{LLMProvider, LLMRequest, LLMResponse, StreamingCallback};
use std::time::Duration;

/// Answers once `release` is notified.
#[derive(Clone)]
struct GatedProvider {
    release: Arc<tokio::sync::Notify>,
    answer: &'static str,
}

#[async_trait::async_trait]
impl LLMProvider for GatedProvider {
    async fn send_message(
        &mut self,
        _request: LLMRequest,
        _streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        self.release.notified().await;
        Ok(create_test_response_text(self.answer))
    }
}

/// A service whose first run spawns a background sub-agent and ends its
/// turn; the sub-agent answers once `release` is notified; the run its
/// result starts answers "Got it".
fn service_with_background_child(
    root: &std::path::Path,
    release: Arc<tokio::sync::Notify>,
) -> (SessionService, Arc<Mutex<SessionManager>>) {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let factory: LlmClientFactory = Arc::new(move |_| {
        let provider: Box<dyn LLMProvider> =
            match calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => Box::new(MockLLMProvider::new(vec![
                    Ok(create_test_response_text("Waiting for the sub-agent.")),
                    Ok(create_test_response(
                        "spawn-1",
                        "spawn_agent",
                        serde_json::json!({
                            "instructions": "Find the parser",
                            "background": true,
                        }),
                        "Let me look in the background.",
                    )),
                ])),
                1 => Box::new(GatedProvider {
                    release: release.clone(),
                    answer: "The parser lives in parser.rs.",
                }),
                _ => Box::new(MockLLMProvider::new(vec![Ok(create_test_response_text(
                    "Got it",
                ))])),
            };
        Ok(provider)
    });

    let events = EventStream::new();
    let persistence = FileSessionPersistence::new_with_root_dir(root.to_path_buf());
    let manager = Arc::new(Mutex::new(SessionManager::new(
        persistence,
        SessionConfig::default(),
        "test-model".to_string(),
        crate::tools::test_registry(),
        events.clone(),
    )));
    let runtime = Arc::new(AgentRuntimeOptions {
        record_path: None,
        playback_path: None,
        fast_playback: false,
        command_executor_factory: Arc::new(|_| {
            Box::new(crate::mocks::create_command_executor_mock())
        }),
        project_manager_factory: Arc::new(|| Box::new(crate::mocks::MockProjectManager::new())),
        llm_client_factory: Some(factory),
    });
    let (service, worker) = SessionService::new(manager.clone(), runtime, events);
    tokio::spawn(worker);
    (service, manager)
}

async fn wait_for<T>(mut check: impl AsyncFnMut() -> Option<T>) -> T {
    for _ in 0..500 {
        if let Some(value) = check().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not reached in time");
}

async fn spawn_result(manager: &Arc<Mutex<SessionManager>>, id: &str) -> Option<SpawnAgentOutput> {
    let manager = manager.lock().await;
    let exec = manager
        .get_session(id)?
        .session
        .tool_executions
        .iter()
        .find(|exec| exec.tool_name == "spawn_agent")?
        .clone();
    serde_json::from_value(exec.result_json).ok()
}

fn text_of(message: &crate::ui::ui_events::MessageData) -> String {
    message
        .fragments
        .iter()
        .filter_map(|f| match f {
            crate::ui::DisplayFragment::PlainText(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn background_result_starts_a_turn_in_the_idle_session() {
    let tmp = tempfile::tempdir().unwrap();
    let release = Arc::new(tokio::sync::Notify::new());
    let (service, manager) = service_with_background_child(tmp.path(), release.clone());
    manager
        .lock()
        .await
        .set_sub_agent_sink(Arc::new(service.clone()));
    let id = service.create_session(None, None).await.unwrap();
    service
        .send_user_message(id.clone(), "Where is the parser?".into(), vec![], None)
        .await
        .unwrap();

    // The parent's turn ends while the sub-agent still runs.
    let snapshot = wait_for(async || {
        let snapshot = service.load_session(id.clone(), None).await.unwrap();
        (snapshot.activity_state.is_terminal() && !snapshot.running_sub_agents.is_empty())
            .then_some(snapshot)
    })
    .await;
    assert_eq!(snapshot.running_sub_agents.len(), 1);
    let running = &snapshot.running_sub_agents[0];
    assert_eq!(running.id, 1);
    assert!(running.background);
    assert_eq!(running.title, "Find the parser");
    let started = spawn_result(&manager, &id).await.unwrap();
    assert_eq!(started.agent_id, Some(1));
    assert!(started.running_in.is_some());

    // The answer arrives as a message and starts a turn.
    release.notify_one();
    wait_for(async || {
        let snapshot = service.load_session(id.clone(), None).await.unwrap();
        let delivered = snapshot.messages.iter().any(|m| {
            m.role == crate::ui::ui_events::MessageRole::User
                && text_of(m).starts_with("[sub-agent #1] finished:")
                && text_of(m).contains("parser.rs")
        });
        let answered = snapshot.messages.last().is_some_and(|m| {
            m.role == crate::ui::ui_events::MessageRole::Assistant && text_of(m) == "Got it"
        });
        (delivered && answered && snapshot.activity_state.is_terminal()).then_some(())
    })
    .await;

    // The stored result now shows the finished card; what the model saw
    // when it spawned the agent stays.
    let finished = spawn_result(&manager, &id).await.unwrap();
    assert!(finished.running_in.is_none());
    assert_eq!(finished.answer, started.answer);
    let card = SubAgentOutput::from_json(finished.ui_output.as_deref().unwrap()).unwrap();
    assert_eq!(card.activity, Some(SubAgentActivity::Completed));
    let snapshot = service.load_session(id.clone(), None).await.unwrap();
    assert!(snapshot.running_sub_agents.is_empty());
}

fn stored_spawn(tool_id: &str) -> agent_core::SerializedToolExecution {
    agent_core::SerializedToolExecution {
        tool_request: crate::tools::ToolRequest {
            id: tool_id.to_string(),
            name: "spawn_agent".to_string(),
            input: serde_json::json!({ "instructions": "look", "background": true }),
            start_offset: None,
            end_offset: None,
        },
        result_json: serde_json::to_value(SpawnAgentOutput::background_started(
            1,
            SubAgentOutput::new().to_json(),
        ))
        .unwrap(),
        tool_name: "spawn_agent".to_string(),
    }
}

fn finished(tool_id: &str, agent_id: u64, answer: &str) -> FinishedSubAgent {
    FinishedSubAgent {
        agent_id,
        tool_id: tool_id.to_string(),
        outcome: FinishedOutcome::Completed {
            answer: answer.to_string(),
        },
        ui_output: String::new(),
    }
}

#[tokio::test]
async fn background_results_queue_while_the_agent_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let (service, manager) = test_service_with_manager(tmp.path());
    let id = service.create_session(None, None).await.unwrap();
    {
        let mut manager = manager.lock().await;
        let instance = manager.get_session_mut(&id).unwrap();
        instance.set_activity_state(crate::session::instance::SessionActivityState::AgentRunning);
    }

    // The run stored each `spawn_agent` call (in memory only here; recording
    // the result reloads the session from disk).
    for (tool_id, agent_id, answer) in [("t1", 1, "first"), ("t2", 2, "second")] {
        manager
            .lock()
            .await
            .get_session_mut(&id)
            .unwrap()
            .session
            .tool_executions
            .push(stored_spawn(tool_id));
        assert!(
            service
                .deliver_sub_agent_result(id.clone(), finished(tool_id, agent_id, answer))
                .await
                .unwrap()
        );
    }

    // Both wait for the running agent's next step, in one message.
    let pending = service.take_pending_message(id).await.unwrap().unwrap();
    assert!(pending.contains("[sub-agent #1] finished:"));
    assert!(pending.contains("[sub-agent #2] finished:"));
}

#[tokio::test]
async fn background_result_for_a_deleted_session_is_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    let (service, _) = test_service_with_manager(tmp.path());
    service
        .deliver_sub_agent_result("gone".into(), finished("t1", 1, "late"))
        .await
        .unwrap();
}

#[tokio::test]
async fn background_result_waits_until_its_call_is_stored() {
    let tmp = tempfile::tempdir().unwrap();
    let (service, manager) = test_service_with_manager(tmp.path());
    let id = service.create_session(None, None).await.unwrap();
    manager
        .lock()
        .await
        .get_session_mut(&id)
        .unwrap()
        .set_activity_state(crate::session::instance::SessionActivityState::AgentRunning);

    // The run is still working on the batch with the `spawn_agent` call.
    assert!(
        !service
            .deliver_sub_agent_result(id.clone(), finished("t1", 1, "early"))
            .await
            .unwrap()
    );
    assert_eq!(
        service.take_pending_message(id.clone()).await.unwrap(),
        None
    );

    // Once the run ended, the result is delivered regardless.
    manager
        .lock()
        .await
        .get_session_mut(&id)
        .unwrap()
        .set_activity_state(crate::session::instance::SessionActivityState::Idle);
    assert!(
        service
            .deliver_sub_agent_result(id.clone(), finished("t1", 1, "early"))
            .await
            .unwrap()
    );
}

#[test]
fn a_background_agent_of_an_earlier_process_shows_as_lost() {
    use crate::tools::core::{Render, ResourcesTracker};

    let mut output = SpawnAgentOutput::background_started(3, SubAgentOutput::new().to_json());
    let mut tracker = ResourcesTracker::new();
    let live = SubAgentOutput::from_json(&output.render_for_ui(&mut tracker)).unwrap();
    assert_eq!(live.activity, Some(SubAgentActivity::WaitingForLlm));

    output.running_in = Some("another-process".into());
    let lost = SubAgentOutput::from_json(&output.render_for_ui(&mut tracker)).unwrap();
    assert_eq!(lost.activity, Some(SubAgentActivity::Failed));
    assert!(lost.error.unwrap().contains("quit"));
    // What the model saw is unchanged.
    assert!(
        output
            .render(&mut tracker)
            .contains("started in the background")
    );
}
