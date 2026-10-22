use super::*;
use crate::mocks::{MockLLMProvider, MockProjectManager, MockUI, PendingLLMProvider};
use crate::session::service::LlmClientFactory;
use std::time::Duration;

fn runner(factory: LlmClientFactory) -> (Arc<DefaultSubAgentRunner>, Arc<MockUI>) {
    let ui = Arc::new(MockUI::default());
    let runner = DefaultSubAgentRunner::new(
        SessionModelConfig::new("test-sub-agent".into()),
        SessionConfig::default(),
        Arc::default(),
        Arc::default(),
        ui.clone(),
        None,
        ToolPermissions::default(),
        crate::tools::test_registry(),
        None,
        None,
    )
    .with_llm_client_factory(factory)
    .with_project_manager_factory(Arc::new(|| Box::new(MockProjectManager::new())));
    (Arc::new(runner), ui)
}

fn failing_provider() -> LlmClientFactory {
    Arc::new(|_| {
        Ok(Box::new(MockLLMProvider::new(vec![Err(anyhow::anyhow!(
            "fatal provider failure"
        ))])))
    })
}

/// Start the child on its own task and wait until it is parked inside the
/// provider.
async fn parked_child(
    runner: &Arc<DefaultSubAgentRunner>,
    entered: &tokio::sync::Notify,
) -> tokio::task::JoinHandle<Result<SubAgentResult>> {
    let task = tokio::spawn({
        let runner = runner.clone();
        async move {
            runner
                .run("child", "inspect".into(), SubAgentMode::ReadOnly, false)
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    task
}

/// The child must end with an error within a short while, without a
/// provider chunk ever arriving.
async fn assert_ends_cancelled(mut task: tokio::task::JoinHandle<Result<SubAgentResult>>) {
    let ended = tokio::time::timeout(Duration::from_millis(250), &mut task).await;
    if ended.is_err() {
        task.abort();
        let _ = task.await;
    }
    assert!(
        matches!(ended, Ok(Ok(Err(_)))),
        "cancellation waited for another provider chunk"
    );
}

#[tokio::test]
async fn sub_agent_failure_unregisters_and_publishes_terminal_output() {
    let (runner, ui) = runner(failing_provider());
    let result = runner
        .run("child", "inspect".into(), SubAgentMode::ReadOnly, false)
        .await;
    assert!(result.is_err());
    assert!(
        !runner.cancellation_registry.cancel("child"),
        "failed child remained registered as busy"
    );

    let events = ui.events();
    let last = events.iter().rev().find_map(|event| match event {
        UiEvent::UpdateToolStatus { status, output, .. } => Some((status, output)),
        _ => None,
    });
    let Some((ToolStatus::Error, Some(json))) = last else {
        panic!("failed child did not publish terminal structured output")
    };
    let output = SubAgentOutput::from_json(json).unwrap();
    assert_eq!(output.activity, Some(SubAgentActivity::Failed));
    assert!(output.error.unwrap().contains("fatal provider failure"));
}

#[tokio::test]
async fn sub_agent_construction_failure_unregisters() {
    let (runner, _) = runner(Arc::new(|_| anyhow::bail!("constructor failed")));
    assert!(
        runner
            .run("child", "inspect".into(), SubAgentMode::ReadOnly, false)
            .await
            .is_err()
    );
    assert!(!runner.cancellation_registry.cancel("child"));
}

#[tokio::test]
async fn sub_agent_drop_unregisters_even_without_a_return_value() {
    let provider = PendingLLMProvider::default();
    let entered = provider.entered.clone();
    let (runner, _) = runner(provider.into_factory());
    let task = parked_child(&runner, &entered).await;
    task.abort();
    let _ = task.await;
    assert!(
        !runner.cancellation_registry.cancel("child"),
        "aborted child leaked its registration"
    );
}

#[tokio::test]
async fn sub_agent_cancel_wakes_a_provider_without_chunks() {
    let provider = PendingLLMProvider::default();
    let entered = provider.entered.clone();
    let (runner, _) = runner(provider.into_factory());
    let task = parked_child(&runner, &entered).await;
    assert!(runner.cancellation_registry.cancel("child"));
    assert_ends_cancelled(task).await;
    assert!(!runner.cancellation_registry.cancel("child"));
}

#[tokio::test]
async fn sub_agent_parent_cancel_wakes_a_provider_without_chunks() {
    let provider = PendingLLMProvider::default();
    let entered = provider.entered.clone();
    let (runner, _) = runner(provider.into_factory());
    let parent = runner.parent_cancellation.clone();
    let task = parked_child(&runner, &entered).await;
    parent.cancel();
    assert_ends_cancelled(task).await;
    assert!(!runner.cancellation_registry.cancel("child"));
}

#[tokio::test]
async fn sub_agent_tool_retains_structured_failure_output() {
    use crate::tools::impls::spawn_agent::{SpawnAgentInput, SpawnAgentTool};
    use tools_core::Tool;

    let (runner, ui) = runner(failing_provider());
    let mut services = crate::tools::ToolServices {
        project_manager: Arc::new(MockProjectManager::new()),
        plan: None,
        ui: Some(ui),
        sub_agent_runner: Some(runner),
        wakeups: None,
        pty_sessions: None,
        terminal_interrupts: None,
        browser_sessions: None,
        session_source: None,
        mcp_resources: None,
    };
    let executor = command_executor::DefaultCommandExecutor;
    let mut context = tools_core::ToolContext {
        command_executor: &executor,
        tool_id: Some("child".into()),
        session_id: None,
        permission_handler: None,
        extensions: Some(&mut services),
    };
    let mut input = SpawnAgentInput {
        instructions: "inspect".into(),
        require_file_references: false,
        mode: "read_only".into(),
        background: false,
    };
    let output = SpawnAgentTool
        .execute(&mut context, &mut input)
        .await
        .unwrap();
    assert!(output.error.is_some());
    let json = output
        .ui_output
        .expect("persist the structured failed child, not just its error text");
    assert_eq!(
        SubAgentOutput::from_json(&json).unwrap().activity,
        Some(SubAgentActivity::Failed)
    );
}

/// A read-only probe tool whose success the sub-agent's UI must remember.
struct Probe;

#[derive(serde::Serialize, serde::Deserialize)]
struct Written;

impl tools_core::ToolResult for Written {
    fn is_success(&self) -> bool {
        true
    }
}

impl tools_core::Render for Written {
    fn status(&self) -> String {
        "written".into()
    }

    fn render(&self, _: &mut tools_core::ResourcesTracker) -> String {
        "changed test resource".into()
    }
}

#[async_trait::async_trait]
impl tools_core::Tool for Probe {
    type Input = serde_json::Value;
    type Output = Written;

    fn spec(&self) -> tools_core::ToolSpec {
        tools_core::ToolSpec {
            name: "probe".into(),
            description: "test".into(),
            parameters_schema: serde_json::json!({"type":"object"}),
            annotations: None,
            capabilities: tools_core::ToolSpec::capabilities(&[ToolScope::SubAgentReadOnly.tag()]),
            multiline_params: &[],
            hidden: false,
            title_template: None,
        }
    }

    async fn execute<'a>(
        &self,
        _: &mut tools_core::ToolContext<'a>,
        _: &mut Self::Input,
    ) -> Result<Written> {
        Ok(Written)
    }
}

#[tokio::test]
async fn sub_agent_keeps_completed_child_evidence_when_later_request_fails() {
    // Served in reverse: first a tool call, then a fatal failure.
    let provider = MockLLMProvider::new(vec![
        Err(anyhow::anyhow!("fatal provider failure after tool")),
        Ok(llm::LLMResponse {
            content: vec![llm::ContentBlock::new_tool_use(
                "inner",
                "probe",
                serde_json::json!({}),
            )],
            usage: llm::Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            rate_limit_info: None,
        }),
    ])
    .streaming();
    let (mut runner, _) = runner(provider.into_factory());
    let mut registry = tools_core::ToolRegistry::new();
    registry.register(Box::new(Probe));
    Arc::get_mut(&mut runner).unwrap().tool_registry = Arc::new(registry);

    let error = runner
        .run("child", "inspect".into(), SubAgentMode::ReadOnly, false)
        .await
        .unwrap_err();
    let failure = error.downcast_ref::<SubAgentFailure>().unwrap();
    let output = SubAgentOutput::from_json(&failure.ui_output).unwrap();
    assert_eq!(output.activity, Some(SubAgentActivity::Failed));
    assert_eq!(output.tools.len(), 1);
    assert_eq!(output.tools[0].name, "probe");
    assert_eq!(output.tools[0].status, SubAgentToolStatus::Success);
    assert_eq!(
        output
            .usage
            .expect("usage before failure must survive")
            .output_tokens,
        5
    );
}

#[tokio::test]
async fn sub_agent_factory_panic_is_a_terminal_failure() {
    let (runner, ui) = runner(Arc::new(|_| panic!("factory panicked")));
    let error = runner
        .run("child", "inspect".into(), SubAgentMode::ReadOnly, false)
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<SubAgentFailure>().is_some());
    assert!(!runner.cancellation_registry.cancel("child"));
    assert!(ui.events().iter().any(|event| matches!(
        event,
        UiEvent::UpdateToolStatus {
            status: ToolStatus::Error,
            ..
        }
    )));
}

#[test]
fn sub_agent_success_clears_a_recovered_stream_error() {
    let ui = SubAgentUiAdapter::new(
        Arc::new(MockUI::default()),
        "child".into(),
        tools_core::RunCancellation::default(),
        crate::tools::test_registry(),
        None,
    );
    ui.set_error("temporary stream error".into());
    ui.set_response("recovered".into());
    let output = SubAgentOutput::from_json(&ui.get_final_output()).unwrap();
    assert_eq!(output.activity, Some(SubAgentActivity::Completed));
    assert!(
        output.error.is_none(),
        "successful retry retained an error banner"
    );
}

#[test]
fn sub_agent_old_registration_cannot_remove_its_replacement() {
    let registry = SubAgentCancellationRegistry::default();
    let parent = tools_core::RunCancellation::default();
    let old = registry.register_run("child", &parent);
    let new = registry.register_run("child", &parent);
    assert!(old.token.is_cancelled());
    drop(old);
    assert!(registry.cancel("child"));
    assert!(new.token.is_cancelled());
    drop(new);
    assert!(!registry.cancel("child"));
}

/// Records delivered background results.
#[derive(Default)]
struct RecordingSink {
    finished: Mutex<Vec<(String, FinishedSubAgent)>>,
    delivered: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl SubAgentSink for RecordingSink {
    async fn finished(&self, session_id: &str, finished: FinishedSubAgent) {
        self.finished
            .lock()
            .unwrap()
            .push((session_id.to_string(), finished));
        self.delivered.notify_one();
    }
}

impl RecordingSink {
    async fn next(&self) -> (String, FinishedSubAgent) {
        loop {
            if let Some(first) = {
                let mut finished = self.finished.lock().unwrap();
                (!finished.is_empty()).then(|| finished.remove(0))
            } {
                return first;
            }
            tokio::time::timeout(Duration::from_secs(5), self.delivered.notified())
                .await
                .expect("background result was not delivered");
        }
    }
}

struct Background {
    runner: Arc<DefaultSubAgentRunner>,
    ui: Arc<MockUI>,
    sink: Arc<RecordingSink>,
    running: Arc<RunningSubAgents>,
    session_cancellation: tools_core::RunCancellation,
}

fn background_runner(factory: LlmClientFactory) -> Background {
    let (runner, ui) = runner(factory);
    let sink = Arc::new(RecordingSink::default());
    let running = Arc::new(RunningSubAgents::default());
    let session_cancellation = tools_core::RunCancellation::default();
    let runner = Arc::new(
        Arc::unwrap_or_clone(runner)
            .with_running("session".into(), running.clone())
            .with_background(Some(BackgroundSupport {
                session_id: "session".into(),
                ui: ui.clone(),
                cancellation: session_cancellation.clone(),
                sink: sink.clone(),
            })),
    );
    Background {
        runner,
        ui,
        sink,
        running,
        session_cancellation,
    }
}

fn answering_provider(answer: &'static str) -> LlmClientFactory {
    Arc::new(move |_| {
        Ok(Box::new(MockLLMProvider::new(vec![Ok(
            crate::mocks::create_test_response_text(answer),
        )])))
    })
}

fn running_events(ui: &MockUI) -> Vec<Vec<RunningSubAgentInfo>> {
    ui.events()
        .into_iter()
        .filter_map(|event| match event {
            UiEvent::UpdateRunningSubAgents { agents, .. } => Some(agents),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn background_sub_agent_delivers_its_answer_later() {
    let bg = background_runner(answering_provider("Found it in parser.rs"));
    let started = bg
        .runner
        .start_background(
            "tool-1",
            "Find the parser".into(),
            SubAgentMode::ReadOnly,
            false,
        )
        .await
        .unwrap();
    assert_eq!(started.agent_id, 1);
    let card = SubAgentOutput::from_json(&started.ui_output).unwrap();
    assert_eq!(card.agent_id, Some(1));
    assert!(card.background);

    let (session_id, finished) = bg.sink.next().await;
    assert_eq!(session_id, "session");
    assert_eq!(finished.agent_id, 1);
    assert_eq!(finished.tool_id, "tool-1");
    assert_eq!(
        finished.outcome,
        FinishedOutcome::Completed {
            answer: "Found it in parser.rs".into()
        }
    );
    assert!(finished.message().starts_with("[sub-agent #1] finished:"));
    let card = SubAgentOutput::from_json(&finished.ui_output).unwrap();
    assert_eq!(card.activity, Some(SubAgentActivity::Completed));
    assert!(card.background);

    // Listed while running, taken off the list when done.
    let listed = running_events(&bg.ui);
    assert!(listed.first().is_some_and(|agents| agents.len() == 1
        && agents[0].background
        && agents[0].title == "Find the parser"));
    assert!(listed.last().is_some_and(Vec::is_empty));
    assert!(bg.running.list().is_empty());
}

#[tokio::test]
async fn background_sub_agent_outlives_the_parent_run() {
    let provider = PendingLLMProvider::default();
    let entered = provider.entered.clone();
    let bg = background_runner(provider.into_factory());
    bg.runner
        .start_background("tool-1", "inspect".into(), SubAgentMode::ReadOnly, false)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();

    // Stopping the parent's turn leaves the background agent running.
    bg.runner.parent_cancellation.cancel();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(bg.running.list().len(), 1);
    assert!(bg.sink.finished.lock().unwrap().is_empty());

    // Its own cancel ends it, reported as cancelled.
    assert!(bg.runner.cancellation_registry.cancel("tool-1"));
    let (_, finished) = bg.sink.next().await;
    assert!(matches!(
        finished.outcome,
        FinishedOutcome::Cancelled { .. }
    ));
    assert!(finished.message().starts_with("[sub-agent #1] cancelled:"));
    assert!(bg.running.list().is_empty());
}

#[tokio::test]
async fn background_sub_agents_end_with_the_session() {
    let provider = PendingLLMProvider::default();
    let entered = provider.entered.clone();
    let bg = background_runner(provider.into_factory());
    for tool_id in ["tool-1", "tool-2"] {
        bg.runner
            .start_background(tool_id, "inspect".into(), SubAgentMode::ReadOnly, false)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
    }
    let ids: Vec<u64> = bg.running.list().iter().map(|a| a.id).collect();
    assert_eq!(ids, vec![1, 2]);

    bg.session_cancellation.cancel();
    for _ in 0..2 {
        let (_, finished) = bg.sink.next().await;
        assert!(matches!(
            finished.outcome,
            FinishedOutcome::Cancelled { .. }
        ));
    }
    assert!(bg.running.list().is_empty());
}

#[tokio::test]
async fn background_failure_is_delivered_as_failed() {
    let bg = background_runner(failing_provider());
    bg.runner
        .start_background("tool-1", "inspect".into(), SubAgentMode::ReadOnly, false)
        .await
        .unwrap();
    let (_, finished) = bg.sink.next().await;
    let FinishedOutcome::Failed { error } = &finished.outcome else {
        panic!("expected a failure, got {:?}", finished.outcome);
    };
    assert!(error.contains("fatal provider failure"));
    assert!(finished.message().starts_with("[sub-agent #1] failed:"));
}

#[tokio::test]
async fn writing_sub_agents_cannot_run_in_the_background() {
    let bg = background_runner(answering_provider("done"));
    let error = bg
        .runner
        .start_background("tool-1", "edit".into(), SubAgentMode::Default, false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("read-only"));
    assert!(bg.running.list().is_empty());
}

#[tokio::test]
async fn background_needs_a_session_to_report_to() {
    let (runner, _) = runner(answering_provider("done"));
    assert!(
        runner
            .start_background("tool-1", "inspect".into(), SubAgentMode::ReadOnly, false)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn foreground_sub_agents_are_listed_while_they_run() {
    let bg = background_runner(answering_provider("done"));
    let result = bg
        .runner
        .run("tool-1", "inspect".into(), SubAgentMode::ReadOnly, false)
        .await
        .unwrap();
    assert_eq!(result.answer, "done");
    let card = SubAgentOutput::from_json(&result.ui_output).unwrap();
    assert_eq!(card.agent_id, Some(1));
    assert!(!card.background);
    let listed = running_events(&bg.ui);
    assert!(
        listed
            .first()
            .is_some_and(|agents| agents.len() == 1 && !agents[0].background)
    );
    assert!(listed.last().is_some_and(Vec::is_empty));
}
