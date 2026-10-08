//! `/new` and `/handoff` through the service (see
//! [`crate::session::new_context`]): the run that opens the new context,
//! the answer to its target question, and the new session it may hand over
//! to.

use super::*;
use crate::session::new_context::NewContextTarget;

impl SessionService {
    /// Start the run for a message that is a [`NewContextCommand`]. When the
    /// user picks a new session, the run hands the prompt back and a task
    /// holding this service creates that session once the run is done.
    pub(super) async fn start_new_context(
        &self,
        session_id: String,
        message: String,
        command: NewContextCommand,
        attachments: Vec<DraftAttachment>,
        branch_parent_id: Option<NodeId>,
    ) -> Result<()> {
        ensure!(
            attachments.is_empty(),
            "/new and /handoff don't take attachments"
        );
        // `/new` stores no message of its own; its boundary is appended at
        // the end of the active path and cannot branch off an edited message.
        ensure!(
            branch_parent_id.is_none() || matches!(command, NewContextCommand::Handoff { .. }),
            "/new can't replace an edited message"
        );
        let service = self.clone();
        let stores_request = matches!(command, NewContextCommand::Handoff { .. });
        self.call_session(session_id.clone(), move |ctx| async move {
            let (new_session, handed_over) = tokio::sync::oneshot::channel();
            start_new_context_impl(
                &ctx,
                &session_id,
                &message,
                command,
                branch_parent_id,
                new_session,
            )
            .await?;
            // Spawned here, on the worker's runtime: callers such as GPUI
            // use the service from their own executor.
            tokio::spawn(async move {
                // Dropped without a prompt: the context stayed in this
                // session, or the run was stopped or failed. Then a
                // `/handoff` request nothing answered is taken back.
                let Ok(prompt) = handed_over.await else {
                    if stores_request
                        && let Err(error) = service.retract_unanswered_handoff(&session_id).await
                    {
                        warn!("Failed to take back the handoff request in {session_id}: {error:#}");
                    }
                    return;
                };
                if let Err(error) = service.continue_in_new_session(&session_id, prompt).await {
                    warn!("Failed to continue {session_id} in a new session: {error:#}");
                    service.events.publish_ui(
                        &session_id,
                        UiEvent::DisplayError {
                            message: format!("Failed to start the new session: {error:#}"),
                        },
                    );
                }
            });
            Ok(())
        })
        .await
    }

    /// Answer the open target question of `/new` or `/handoff`.
    pub async fn respond_new_context_target(
        &self,
        session_id: String,
        request_id: String,
        target: NewContextTarget,
    ) -> Result<()> {
        self.call_control(move |ctx| async move {
            let manager = ctx.manager.lock().await;
            let instance = manager
                .get_session(&session_id)
                .ok_or_else(|| anyhow!("Session {session_id} not found"))?;
            instance
                .pending_new_context_target
                .resolve(&request_id, target);
            Ok(())
        })
        .await
    }

    /// Have the agent write a handoff prompt for the composer, if the
    /// session qualifies (see [`SessionManager::claim_handoff_preparation`]).
    /// Fired by the idle timers; the prompt arrives as
    /// [`UiEvent::HandoffPrepared`].
    pub async fn prepare_handoff(&self, session_id: String, threshold_tokens: u64) -> Result<()> {
        self.call_session(session_id.clone(), move |ctx| async move {
            let claimed = ctx
                .manager
                .lock()
                .await
                .claim_handoff_preparation(&session_id, threshold_tokens)?;
            if !claimed {
                return Ok(());
            }
            let options = RunOptions {
                task: RunTask::PrepareHandoff,
                ..Default::default()
            };
            start_agent_impl(&ctx, &session_id, options).await
        })
        .await
    }

    /// The user is active in the session (typing in its composer): postpone
    /// preparing a handoff.
    pub async fn note_user_activity(&self, session_id: String) -> Result<()> {
        self.call_control(move |ctx| async move {
            if let Some(timers) = ctx.manager.lock().await.idle_handoff() {
                timers.touch(&session_id);
            }
            Ok(())
        })
        .await
    }

    /// Remove the session's `/handoff` message if nothing answered it, and
    /// show the transcript without it.
    async fn retract_unanswered_handoff(&self, session_id: &str) -> Result<()> {
        let session_id = session_id.to_string();
        self.call_session(session_id.clone(), move |ctx| async move {
            let mut manager = ctx.manager.lock().await;
            if !manager.retract_unanswered_handoff(&session_id)? {
                return Ok(());
            }
            if let Some(instance) = manager.get_session(&session_id) {
                let transcript = transcript_data(&manager, instance)?;
                ctx.notify_session(
                    &session_id,
                    UiEvent::SetMessages {
                        messages: transcript.messages,
                        session_id: Some(session_id.clone()),
                        tool_results: transcript.tool_results,
                    },
                );
            }
            Ok(())
        })
        .await
    }

    /// Create a session with `from`'s settings whose first user message is
    /// `prompt`, start the agent on it, and tell `from`'s viewers to switch.
    /// Without a prompt the session stays empty, like after `/clear`.
    async fn continue_in_new_session(&self, from: &str, prompt: String) -> Result<()> {
        let to = self.start_fresh_session(from.to_string()).await?;
        let from = from.to_string();
        self.call_session(to.clone(), move |ctx| async move {
            if !prompt.trim().is_empty() {
                let blocks = content_blocks_from(&prompt, &[], None);
                append_and_run(&ctx, &to, &prompt, blocks, &[], None, RunOptions::default())
                    .await?;
            }
            ctx.notify_session(&from, UiEvent::SessionHandedOff { to });
            Ok(())
        })
        .await
    }
}

async fn start_new_context_impl(
    ctx: &ServiceCtx,
    session_id: &str,
    message: &str,
    command: NewContextCommand,
    branch_parent_id: Option<NodeId>,
    new_session: tokio::sync::oneshot::Sender<String>,
) -> Result<()> {
    match command {
        // Nothing of `/new` is stored in this session: the boundary (same
        // session) or the new session's first message carries the prompt.
        NewContextCommand::New { prompt } => {
            let options = RunOptions {
                task: RunTask::NewContext(NewContextRun {
                    prompt: Some(prompt),
                    new_session,
                }),
                ..Default::default()
            };
            start_agent_impl(ctx, session_id, options).await
        }
        // The request to write the handoff rides along with the typed
        // message, so the generation request is the history as it is. An
        // edited message branches off like any other.
        NewContextCommand::Handoff { .. } => {
            let mut blocks = content_blocks_from(message, &[], None);
            blocks.push(llm::ContentBlock::new_text(
                crate::session::new_context::handoff_request(),
            ));
            let options = RunOptions {
                task: RunTask::NewContext(NewContextRun {
                    prompt: None,
                    new_session,
                }),
                ..Default::default()
            };
            append_and_run(
                ctx,
                session_id,
                message,
                blocks,
                &[],
                branch_parent_id,
                options,
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{test_service_with_llm, test_service_with_manager};
    use super::*;
    use crate::mocks::MockLLMProvider;
    use crate::session::event_stream::{EventPayload, Subscription};

    fn text_response(text: &str) -> Result<llm::LLMResponse> {
        Ok(llm::LLMResponse {
            content: vec![llm::ContentBlock::new_text(text)],
            // The last request's input, as the idle preparation reads it.
            usage: llm::Usage {
                input_tokens: 100,
                output_tokens: 5,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 900,
            },
            rate_limit_info: None,
        })
    }

    /// A provider answering with `texts` in order, shared across runs.
    fn answering(texts: &[&str]) -> MockLLMProvider {
        // The mock serves its responses as a stack.
        MockLLMProvider::new(texts.iter().rev().map(|text| text_response(text)).collect())
    }

    /// Wait for the first UI event of `session_id` that `pick` accepts.
    async fn next<T>(
        subscription: &mut Subscription,
        session_id: &str,
        mut pick: impl FnMut(UiEvent) -> Option<T>,
    ) -> T {
        loop {
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(10), subscription.recv())
                    .await
                    .expect("the awaited event did not arrive")
                    .unwrap();
            if event.session_id.as_deref() != Some(session_id) {
                continue;
            }
            if let EventPayload::Ui(event) = event.payload
                && let Some(picked) = pick(event)
            {
                return picked;
            }
        }
    }

    async fn answer_target(
        service: &SessionService,
        subscription: &mut Subscription,
        session_id: &str,
        target: NewContextTarget,
    ) {
        let request_id = next(subscription, session_id, |event| match event {
            UiEvent::RequestNewContextTarget { request } => Some(request.request_id),
            _ => None,
        })
        .await;
        service
            .respond_new_context_target(session_id.to_string(), request_id, target)
            .await
            .unwrap();
    }

    async fn idle(subscription: &mut Subscription, session_id: &str) {
        next(subscription, session_id, |event| match event {
            UiEvent::UpdateSessionActivityState {
                activity_state: crate::session::instance::SessionActivityState::Idle,
                ..
            } => Some(()),
            _ => None,
        })
        .await;
    }

    /// The messages on the session's active path, as stored: runs save
    /// through their own manager, so the owner's instance may lag behind.
    fn path(root: &std::path::Path, id: &str) -> Vec<llm::Message> {
        crate::persistence::FileSessionPersistence::new_with_root_dir(root.to_path_buf())
            .load_chat_session(id)
            .unwrap()
            .expect("a stored session")
            .get_active_messages_cloned()
    }

    fn texts(message: &llm::Message) -> Vec<String> {
        match &message.content {
            llm::MessageContent::Text(text) => vec![text.clone()],
            llm::MessageContent::Structured(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    llm::ContentBlock::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect(),
        }
    }

    fn first_text(request: &llm::LLMRequest) -> String {
        texts(&request.messages[0]).join("\n")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn new_in_the_same_session_opens_a_context_with_the_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let llm = answering(&["first answer", "on it"]);
        let (service, _) = test_service_with_llm(tmp.path(), llm.clone().into_factory());
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();
        service
            .send_user_message(id.clone(), "Build X".into(), vec![], None)
            .await
            .unwrap();
        idle(&mut subscription, &id).await;

        service
            .send_user_message(id.clone(), "/new Write the tests".into(), vec![], None)
            .await
            .unwrap();
        answer_target(
            &service,
            &mut subscription,
            &id,
            NewContextTarget::SameSession,
        )
        .await;
        idle(&mut subscription, &id).await;

        let messages = path(tmp.path(), &id);
        assert_eq!(messages.len(), 4, "{messages:?}");
        assert!(messages[2].is_new_context);
        assert_eq!(texts(&messages[2]), ["Write the tests"]);
        assert_eq!(texts(&messages[3]), ["on it"]);
        let requests = llm.get_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].messages.len(), 1);
        assert!(first_text(&requests[1]).starts_with("Write the tests"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn new_without_prompt_only_opens_the_context() {
        let tmp = tempfile::tempdir().unwrap();
        let llm = answering(&[]);
        let (service, _) = test_service_with_llm(tmp.path(), llm.clone().into_factory());
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();

        service
            .send_user_message(id.clone(), "/new".into(), vec![], None)
            .await
            .unwrap();
        answer_target(
            &service,
            &mut subscription,
            &id,
            NewContextTarget::SameSession,
        )
        .await;
        idle(&mut subscription, &id).await;

        let messages = path(tmp.path(), &id);
        assert_eq!(messages.len(), 1);
        assert!(messages[0].is_new_context);
        assert!(llm.get_requests().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_message_queued_during_the_question_opens_an_empty_new_context() {
        let tmp = tempfile::tempdir().unwrap();
        let llm = answering(&["on it"]);
        let (service, _) = test_service_with_llm(tmp.path(), llm.clone().into_factory());
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();

        service
            .send_user_message(id.clone(), "/new".into(), vec![], None)
            .await
            .unwrap();
        let request_id = next(&mut subscription, &id, |event| match event {
            UiEvent::RequestNewContextTarget { request } => Some(request.request_id),
            _ => None,
        })
        .await;
        service
            .queue_user_message(id.clone(), "Write the tests".into(), vec![])
            .await
            .unwrap();
        service
            .respond_new_context_target(id.clone(), request_id, NewContextTarget::SameSession)
            .await
            .unwrap();
        idle(&mut subscription, &id).await;

        let messages = path(tmp.path(), &id);
        assert_eq!(messages.len(), 3, "{messages:?}");
        assert!(messages[0].is_new_context);
        assert_eq!(texts(&messages[1]), ["Write the tests"]);
        assert_eq!(texts(&messages[2]), ["on it"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn new_without_prompt_in_a_new_session_only_creates_it() {
        let tmp = tempfile::tempdir().unwrap();
        let llm = answering(&[]);
        let (service, _) = test_service_with_llm(tmp.path(), llm.clone().into_factory());
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();

        service
            .send_user_message(id.clone(), "/new".into(), vec![], None)
            .await
            .unwrap();
        answer_target(
            &service,
            &mut subscription,
            &id,
            NewContextTarget::NewSession,
        )
        .await;
        let to = next(&mut subscription, &id, |event| match event {
            UiEvent::SessionHandedOff { to } => Some(to),
            _ => None,
        })
        .await;

        assert!(path(tmp.path(), &to).is_empty(), "like /clear");
        assert!(!service.is_session_busy(to).await.unwrap());
        assert!(llm.get_requests().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn new_in_a_new_session_starts_it_with_the_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let llm = answering(&["on it"]);
        let (service, _) = test_service_with_llm(tmp.path(), llm.clone().into_factory());
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();

        service
            .send_user_message(id.clone(), "/new Write the tests".into(), vec![], None)
            .await
            .unwrap();
        answer_target(
            &service,
            &mut subscription,
            &id,
            NewContextTarget::NewSession,
        )
        .await;
        let to = next(&mut subscription, &id, |event| match event {
            UiEvent::SessionHandedOff { to } => Some(to),
            _ => None,
        })
        .await;
        idle(&mut subscription, &to).await;

        assert!(path(tmp.path(), &id).is_empty(), "nothing stored here");
        let messages = path(tmp.path(), &to);
        assert_eq!(messages.len(), 2);
        assert!(!messages[0].is_new_context, "a plain first message");
        assert_eq!(texts(&messages[0]), ["Write the tests"]);
        assert_eq!(texts(&messages[1]), ["on it"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn handoff_in_the_same_session_continues_from_the_generated_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let llm = answering(&["built", "Next: write the tests", "on it"]);
        let (service, _) = test_service_with_llm(tmp.path(), llm.clone().into_factory());
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();
        service
            .send_user_message(id.clone(), "Build X".into(), vec![], None)
            .await
            .unwrap();
        idle(&mut subscription, &id).await;

        service
            .send_user_message(id.clone(), "/handoff focus on tests".into(), vec![], None)
            .await
            .unwrap();
        answer_target(
            &service,
            &mut subscription,
            &id,
            NewContextTarget::SameSession,
        )
        .await;
        idle(&mut subscription, &id).await;

        let messages = path(tmp.path(), &id);
        assert_eq!(messages.len(), 5, "{messages:?}");
        let request = texts(&messages[2]);
        assert_eq!(request[0], "/handoff focus on tests");
        assert!(crate::injection::is_injection(&request[1]));
        assert!(messages[3].is_new_context);
        assert_eq!(texts(&messages[3]), ["Next: write the tests"]);
        assert_eq!(texts(&messages[4]), ["on it"]);

        let requests = llm.get_requests();
        assert_eq!(requests.len(), 3);
        // The generation request is the history, ending with the request.
        assert_eq!(requests[1].messages.len(), 3);
        // The new context starts from the generated prompt alone.
        assert_eq!(requests[2].messages.len(), 1);
        assert!(first_text(&requests[2]).starts_with("Next: write the tests"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn editing_a_message_into_a_handoff_branches_from_it() {
        let tmp = tempfile::tempdir().unwrap();
        let llm = answering(&["built", "answered", "Next: write the tests", "on it"]);
        let (service, _) = test_service_with_llm(tmp.path(), llm.clone().into_factory());
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();
        for message in ["Build X", "/handof typo"] {
            service
                .send_user_message(id.clone(), message.into(), vec![], None)
                .await
                .unwrap();
            idle(&mut subscription, &id).await;
        }
        let typo_node = *crate::persistence::FileSessionPersistence::new_with_root_dir(
            tmp.path().to_path_buf(),
        )
        .load_chat_session(&id)
        .unwrap()
        .unwrap()
        .active_path
        .get(2)
        .unwrap();
        let edit = service
            .start_message_edit(id.clone(), typo_node)
            .await
            .unwrap();

        service
            .send_user_message(
                id.clone(),
                "/handoff focus on tests".into(),
                vec![],
                edit.branch_parent_id,
            )
            .await
            .unwrap();
        answer_target(
            &service,
            &mut subscription,
            &id,
            NewContextTarget::SameSession,
        )
        .await;
        idle(&mut subscription, &id).await;

        let messages = path(tmp.path(), &id);
        assert_eq!(messages.len(), 5, "{messages:?}");
        assert_eq!(texts(&messages[2])[0], "/handoff focus on tests");
        assert!(messages[3].is_new_context);
        // The handoff was written from the branch, without the edited message.
        let generation = &llm.get_requests()[2];
        assert_eq!(generation.messages.len(), 3);
    }

    /// GPUI calls the service from its own executor, outside any tokio
    /// runtime; only the worker runs on one.
    #[test]
    fn commands_can_be_sent_from_outside_the_tokio_runtime() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let (service, _) = runtime
            .block_on(async { test_service_with_llm(tmp.path(), answering(&[]).into_factory()) });
        let id = runtime
            .block_on(service.create_session(None, None))
            .unwrap();

        futures::executor::block_on(service.send_user_message(
            id,
            "/new Write the tests".into(),
            vec![],
            None,
        ))
        .unwrap();
    }

    #[tokio::test]
    async fn new_cannot_replace_an_edited_message() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, _) = test_service_with_manager(tmp.path());
        let id = service.create_session(None, None).await.unwrap();

        let error = service
            .send_user_message(id, "/new Write the tests".into(), vec![], Some(1))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("/new can't"), "{error}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stopped_handoff_takes_its_request_back() {
        let tmp = tempfile::tempdir().unwrap();
        // The handoff is written at once; the run then waits for the target.
        let llm = answering(&["built", "Next: write the tests"]);
        let (service, _) = test_service_with_llm(tmp.path(), llm.into_factory());
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();
        service
            .send_user_message(id.clone(), "Build X".into(), vec![], None)
            .await
            .unwrap();
        idle(&mut subscription, &id).await;

        service
            .send_user_message(id.clone(), "/handoff".into(), vec![], None)
            .await
            .unwrap();
        next(&mut subscription, &id, |event| match event {
            UiEvent::RequestNewContextTarget { .. } => Some(()),
            _ => None,
        })
        .await;
        service.request_stop(id.clone()).await.unwrap();

        // The transcript is replaced without the request.
        let messages = next(&mut subscription, &id, |event| match event {
            UiEvent::SetMessages { messages, .. } => Some(messages),
            _ => None,
        })
        .await;
        assert_eq!(messages.len(), 2);
        assert_eq!(path(tmp.path(), &id).len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stopped_handoff_edit_returns_to_the_edited_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let llm = answering(&["built", "answered", "Next: write the tests"]);
        let (service, _) = test_service_with_llm(tmp.path(), llm.into_factory());
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();
        for message in ["Build X", "/handof typo"] {
            service
                .send_user_message(id.clone(), message.into(), vec![], None)
                .await
                .unwrap();
            idle(&mut subscription, &id).await;
        }
        let before = path(tmp.path(), &id);
        let typo_node = *crate::persistence::FileSessionPersistence::new_with_root_dir(
            tmp.path().to_path_buf(),
        )
        .load_chat_session(&id)
        .unwrap()
        .unwrap()
        .active_path
        .get(2)
        .unwrap();
        let edit = service
            .start_message_edit(id.clone(), typo_node)
            .await
            .unwrap();

        service
            .send_user_message(id.clone(), "/handoff".into(), vec![], edit.branch_parent_id)
            .await
            .unwrap();
        next(&mut subscription, &id, |event| match event {
            UiEvent::RequestNewContextTarget { .. } => Some(()),
            _ => None,
        })
        .await;
        service.request_stop(id.clone()).await.unwrap();
        next(&mut subscription, &id, |event| match event {
            UiEvent::SetMessages { .. } => Some(()),
            _ => None,
        })
        .await;

        let after = path(tmp.path(), &id);
        assert_eq!(after.len(), before.len());
        assert_eq!(texts(&after[2]), ["/handof typo"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_handoff_takes_its_request_back() {
        let tmp = tempfile::tempdir().unwrap();
        // Served as a stack: the first answer, then the failing generation.
        let llm = MockLLMProvider::new(vec![
            Err(anyhow!("provider unavailable")),
            text_response("built"),
        ]);
        let (service, _) = test_service_with_llm(tmp.path(), llm.into_factory());
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();
        service
            .send_user_message(id.clone(), "Build X".into(), vec![], None)
            .await
            .unwrap();
        idle(&mut subscription, &id).await;

        service
            .send_user_message(id.clone(), "/handoff".into(), vec![], None)
            .await
            .unwrap();
        next(&mut subscription, &id, |event| match event {
            UiEvent::SetMessages { .. } => Some(()),
            _ => None,
        })
        .await;

        assert_eq!(path(tmp.path(), &id).len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn handoff_to_a_new_session_answers_here_and_opens_there() {
        let tmp = tempfile::tempdir().unwrap();
        let llm = answering(&["Next: write the tests", "on it"]);
        let (service, _) = test_service_with_llm(tmp.path(), llm.clone().into_factory());
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();

        service
            .send_user_message(id.clone(), "/compact".into(), vec![], None)
            .await
            .unwrap();
        answer_target(
            &service,
            &mut subscription,
            &id,
            NewContextTarget::NewSession,
        )
        .await;
        let to = next(&mut subscription, &id, |event| match event {
            UiEvent::SessionHandedOff { to } => Some(to),
            _ => None,
        })
        .await;
        idle(&mut subscription, &to).await;

        let here = path(tmp.path(), &id);
        assert_eq!(here.len(), 2);
        assert_eq!(texts(&here[0])[0], "/compact");
        assert_eq!(here[1].role, llm::MessageRole::Assistant);
        assert_eq!(texts(&here[1]), ["Next: write the tests"]);
        let there = path(tmp.path(), &to);
        assert_eq!(texts(&there[0]), ["Next: write the tests"]);
        assert_eq!(texts(&there[1]), ["on it"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stopping_during_the_target_question_cancels_the_command() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, _) = test_service_with_llm(tmp.path(), answering(&[]).into_factory());
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();

        service
            .send_user_message(id.clone(), "/new Write the tests".into(), vec![], None)
            .await
            .unwrap();
        let request_id = next(&mut subscription, &id, |event| match event {
            UiEvent::RequestNewContextTarget { request } => Some(request.request_id),
            _ => None,
        })
        .await;
        let snapshot = service.load_session(id.clone(), None).await.unwrap();
        assert_eq!(
            snapshot
                .pending_new_context_target
                .map(|request| request.request_id),
            Some(request_id.clone())
        );

        service.request_stop(id.clone()).await.unwrap();
        let resolved = next(&mut subscription, &id, |event| match event {
            UiEvent::NewContextTargetResolved { request_id } => Some(request_id),
            _ => None,
        })
        .await;
        assert_eq!(resolved, request_id);
        idle(&mut subscription, &id).await;
        assert!(path(tmp.path(), &id).is_empty());
    }

    #[tokio::test]
    async fn commands_are_neither_queued_nor_sent_with_attachments() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, _) = test_service_with_manager(tmp.path());
        let id = service.create_session(None, None).await.unwrap();

        let queued = service
            .queue_user_message(id.clone(), "/handoff".into(), vec![])
            .await;
        assert!(queued.is_err());
        let attachment = DraftAttachment::Text {
            content: "notes".into(),
        };
        let sent = service
            .send_user_message(id, "/new with notes".into(), vec![attachment], None)
            .await;
        assert!(sent.unwrap_err().to_string().contains("attachments"));
    }

    async fn prepared(subscription: &mut Subscription, session_id: &str) -> String {
        next(subscription, session_id, |event| match event {
            UiEvent::HandoffPrepared { prompt } => Some(prompt),
            _ => None,
        })
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prepare_handoff_offers_a_prompt_without_touching_the_history() {
        let tmp = tempfile::tempdir().unwrap();
        let llm = answering(&["built", "Next: write the tests"]);
        let (service, _) = test_service_with_llm(tmp.path(), llm.clone().into_factory());
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();
        service
            .send_user_message(id.clone(), "Build X".into(), vec![], None)
            .await
            .unwrap();
        idle(&mut subscription, &id).await;

        service.prepare_handoff(id.clone(), 1000).await.unwrap();

        assert_eq!(
            prepared(&mut subscription, &id).await,
            "Next: write the tests"
        );
        idle(&mut subscription, &id).await;
        assert_eq!(path(tmp.path(), &id).len(), 2, "the history is unchanged");
        let request = &llm.get_requests()[1];
        assert_eq!(request.messages.len(), 3);
        let appended = texts(request.messages.last().unwrap()).join("\n");
        assert!(appended.starts_with("<handoff-request>"), "{appended}");
        assert!(appended.contains("[cancel handoff]"), "{appended}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancelled_preparation_offers_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let llm = answering(&["Which option do you want?", "  [cancel handoff]\n"]);
        let (service, _) = test_service_with_llm(tmp.path(), llm.clone().into_factory());
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();
        service
            .send_user_message(id.clone(), "Build X".into(), vec![], None)
            .await
            .unwrap();
        idle(&mut subscription, &id).await;

        service.prepare_handoff(id.clone(), 1000).await.unwrap();

        next(&mut subscription, &id, |event| match event {
            UiEvent::HandoffPrepared { prompt } => panic!("offered {prompt:?}"),
            UiEvent::UpdateSessionActivityState {
                activity_state: crate::session::instance::SessionActivityState::Idle,
                ..
            } => Some(()),
            _ => None,
        })
        .await;
        assert_eq!(llm.get_requests().len(), 2);
        assert_eq!(path(tmp.path(), &id).len(), 2, "the history is unchanged");
        // The session's state counts as handled: no second attempt.
        service.prepare_handoff(id.clone(), 1000).await.unwrap();
        assert!(!service.is_session_busy(id.clone()).await.unwrap());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_preparation_leaves_the_session_idle() {
        let tmp = tempfile::tempdir().unwrap();
        // Served as a stack: the turn's answer first, then the failure.
        let llm = MockLLMProvider::new(vec![
            Err(anyhow!("provider unavailable")),
            text_response("built"),
        ]);
        let (service, manager) = test_service_with_llm(tmp.path(), llm.into_factory());
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();
        service
            .send_user_message(id.clone(), "Build X".into(), vec![], None)
            .await
            .unwrap();
        idle(&mut subscription, &id).await;

        service.prepare_handoff(id.clone(), 1000).await.unwrap();
        next(&mut subscription, &id, |event| match event {
            UiEvent::UpdateSessionActivityState { activity_state, .. } => match activity_state {
                crate::session::instance::SessionActivityState::Idle => Some(()),
                crate::session::instance::SessionActivityState::Errored { message } => {
                    panic!("errored: {message}")
                }
                _ => None,
            },
            _ => None,
        })
        .await;

        let state = manager
            .lock()
            .await
            .get_session(&id)
            .unwrap()
            .get_activity_state();
        assert_eq!(state, crate::session::instance::SessionActivityState::Idle);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_preparation_that_answered_a_queued_message_rearms_the_timer() {
        let tmp = tempfile::tempdir().unwrap();
        let llm = answering(&[
            "built",
            "Next: write the tests",
            "answered",
            "Next: ship it",
        ]);
        let (service, manager) = test_service_with_llm(tmp.path(), llm.into_factory());
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();
        service
            .send_user_message(id.clone(), "Build X".into(), vec![], None)
            .await
            .unwrap();
        idle(&mut subscription, &id).await;
        manager.lock().await.set_idle_handoff(
            crate::session::idle_handoff::IdleHandoffTimers::new(
                service.clone(),
                std::time::Duration::from_millis(50),
                Arc::new(|| 1000),
            ),
        );

        // A message arrives while the preparation runs: it gets answered.
        service.prepare_handoff(id.clone(), 1000).await.unwrap();
        service
            .queue_user_message(id.clone(), "One more thing".into(), vec![])
            .await
            .unwrap();

        // The answered state gets its own preparation.
        assert_eq!(prepared(&mut subscription, &id).await, "Next: ship it");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prepare_handoff_skips_an_empty_new_context() {
        let tmp = tempfile::tempdir().unwrap();
        let llm = answering(&["built"]);
        let (service, _) = test_service_with_llm(tmp.path(), llm.clone().into_factory());
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();
        service
            .send_user_message(id.clone(), "Build X".into(), vec![], None)
            .await
            .unwrap();
        idle(&mut subscription, &id).await;
        service
            .send_user_message(id.clone(), "/new".into(), vec![], None)
            .await
            .unwrap();
        answer_target(
            &service,
            &mut subscription,
            &id,
            NewContextTarget::SameSession,
        )
        .await;
        idle(&mut subscription, &id).await;

        // The 1000 tokens were used by the context before the boundary.
        service.prepare_handoff(id.clone(), 1000).await.unwrap();
        assert!(!service.is_session_busy(id).await.unwrap());
        assert_eq!(llm.get_requests().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prepare_handoff_skips_an_unanswered_errored_session() {
        let tmp = tempfile::tempdir().unwrap();
        // Served as a stack: the first answer, then a failing turn.
        let llm = MockLLMProvider::new(vec![
            Err(anyhow!("provider unavailable")),
            text_response("built"),
        ]);
        let (service, manager) = test_service_with_llm(tmp.path(), llm.clone().into_factory());
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();
        service
            .send_user_message(id.clone(), "Build X".into(), vec![], None)
            .await
            .unwrap();
        idle(&mut subscription, &id).await;
        service
            .send_user_message(id.clone(), "And Y".into(), vec![], None)
            .await
            .unwrap();
        next(&mut subscription, &id, |event| match event {
            UiEvent::UpdateSessionActivityState {
                activity_state: crate::session::instance::SessionActivityState::Errored { .. },
                ..
            } => Some(()),
            _ => None,
        })
        .await;

        service.prepare_handoff(id.clone(), 1000).await.unwrap();
        assert!(!service.is_session_busy(id.clone()).await.unwrap());
        assert!(matches!(
            manager
                .lock()
                .await
                .get_session(&id)
                .unwrap()
                .get_activity_state(),
            crate::session::instance::SessionActivityState::Errored { .. }
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prepare_handoff_skips_short_or_already_prepared_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let llm = answering(&["built", "Next: write the tests"]);
        let (service, _) = test_service_with_llm(tmp.path(), llm.clone().into_factory());
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();
        service
            .send_user_message(id.clone(), "Build X".into(), vec![], None)
            .await
            .unwrap();
        idle(&mut subscription, &id).await;

        // The last request's input was 1000 tokens.
        service.prepare_handoff(id.clone(), 1001).await.unwrap();
        assert!(!service.is_session_busy(id.clone()).await.unwrap());

        service.prepare_handoff(id.clone(), 1000).await.unwrap();
        prepared(&mut subscription, &id).await;
        idle(&mut subscription, &id).await;
        service.prepare_handoff(id.clone(), 1000).await.unwrap();
        assert!(!service.is_session_busy(id.clone()).await.unwrap());
        assert_eq!(llm.get_requests().len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_idle_timer_prepares_a_handoff_after_a_run() {
        let tmp = tempfile::tempdir().unwrap();
        let llm = answering(&["built", "Next: write the tests"]);
        let (service, manager) = test_service_with_llm(tmp.path(), llm.into_factory());
        manager.lock().await.set_idle_handoff(
            crate::session::idle_handoff::IdleHandoffTimers::new(
                service.clone(),
                std::time::Duration::from_millis(50),
                Arc::new(|| 1000),
            ),
        );
        let id = service.create_session(None, None).await.unwrap();
        let mut subscription = service.subscribe();

        service
            .send_user_message(id.clone(), "Build X".into(), vec![], None)
            .await
            .unwrap();

        assert_eq!(
            prepared(&mut subscription, &id).await,
            "Next: write the tests"
        );
    }
}
