use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tools_core::{Render, Tool, ToolResult, ToolSpec};

#[derive(serde::Serialize, serde::Deserialize)]
struct Output(String);

impl ToolResult for Output {
    fn is_success(&self) -> bool {
        true
    }
}

impl Render for Output {
    fn status(&self) -> String {
        "done".into()
    }

    fn render(&self, _: &mut ResourcesTracker) -> String {
        self.0.clone()
    }
}

struct Probe {
    calls: Arc<Mutex<Vec<String>>>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    capabilities: Vec<std::borrow::Cow<'static, str>>,
}

#[async_trait::async_trait]
impl Tool for Probe {
    type Input = serde_json::Value;
    type Output = Output;
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "probe".into(),
            description: "test".into(),
            parameters_schema: json!({"type":"object"}),
            annotations: None,
            capabilities: self.capabilities.clone(),
            multiline_params: &[],
            hidden: false,
            title_template: None,
        }
    }

    async fn execute<'a>(
        &self,
        _: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Output> {
        let id = input["id"].as_str().unwrap().to_string();
        self.calls.lock().unwrap().push(id.clone());
        if input["wait"] == true {
            self.entered.notify_one();
            self.release.notified().await;
        }
        if input["rewrite"] == true {
            input["formatted"] = true.into();
        }
        Ok(Output(format!("result for {id}")))
    }
}

struct Fixture {
    agent: AgentRuntime,
    saved: Capture,
    calls: Arc<Mutex<Vec<String>>>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

fn probe_registry(f: &Fixture, capabilities: &[&str]) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(Probe {
        calls: f.calls.clone(),
        entered: f.entered.clone(),
        release: f.release.clone(),
        capabilities: capabilities.iter().map(|c| c.to_string().into()).collect(),
    }));
    registry
}

fn fixture(requests: &[ToolRequest]) -> Fixture {
    let (agent, saved) = runtime();
    let mut f = Fixture {
        agent,
        saved,
        calls: Arc::new(Mutex::new(vec![])),
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    f.agent.registry = Arc::new(probe_registry(&f, &["test"]));
    f.agent.tool_capability = "test".into();
    let agent = &mut f.agent;
    agent
        .append_message(Message::new_assistant_content(
            requests
                .iter()
                .map(|r| ContentBlock::new_tool_use(&r.id, &r.name, r.input.clone()))
                .collect(),
        ))
        .unwrap();
    f
}

fn request(id: &str, wait: bool) -> ToolRequest {
    ToolRequest {
        id: id.into(),
        name: "probe".into(),
        input: json!({"id":id, "wait":wait}),
        start_offset: None,
        end_offset: None,
    }
}

struct Parallel(Vec<usize>);

impl ToolDispatchPolicy for Parallel {
    fn parallel_indices(&self, _: &[ToolRequest]) -> Vec<usize> {
        self.0.clone()
    }
}

struct Observer {
    attempts: Arc<AtomicUsize>,
    successes: Arc<Mutex<Vec<ToolRequest>>>,
    intercept: bool,
}

impl ToolInterceptor for Observer {
    fn try_intercept(
        &self,
        _: &ToolRequest,
        _: &mut LoopCtx,
    ) -> Option<Result<Box<dyn tools_core::AnyOutput>>> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        self.intercept
            .then(|| Ok(Box::new(Output("intercepted".into())) as Box<dyn tools_core::AnyOutput>))
    }

    fn after_tool_success(&self, request: &ToolRequest, _: &mut LoopCtx) {
        self.successes.lock().unwrap().push(request.clone());
    }
}

#[tokio::test]
async fn dispatch_interceptors_cannot_bypass_scope_or_permission_checks() {
    for restricted_scope in [true, false] {
        let requests = vec![request("one", false)];
        let mut f = fixture(&requests);
        let attempts = Arc::new(AtomicUsize::new(0));
        f.agent.hooks.interceptors.push(Box::new(Observer {
            attempts: attempts.clone(),
            successes: Arc::default(),
            intercept: true,
        }));
        if restricted_scope {
            f.agent.tool_capability = "other".into();
        } else {
            f.agent.permissions =
                tools_core::ToolPermissions::new(tools_core::PermissionTier::AllTools);
        }
        f.agent.manage_tool_execution(&requests).await.unwrap();
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            0,
            "mandatory checks must precede interception"
        );
        assert!(f.calls.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn dispatch_parallel_hooks_and_formatted_inputs_match_sequential_contract() {
    let mut requests = vec![request("one", false), request("two", false)];
    for request in &mut requests {
        request.input["rewrite"] = true.into();
    }
    let mut f = fixture(&requests);
    f.agent.hooks.dispatch = Box::new(Parallel(vec![0, 1]));
    let attempts = Arc::new(AtomicUsize::new(0));
    let successes = Arc::new(Mutex::new(vec![]));
    f.agent.hooks.interceptors.push(Box::new(Observer {
        attempts: attempts.clone(),
        successes: successes.clone(),
        intercept: false,
    }));
    f.agent.manage_tool_execution(&requests).await.unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    let successes = successes.lock().unwrap();
    assert_eq!(successes.len(), 2);
    assert!(successes.iter().all(|r| r.input["formatted"] == true));
    let saved = f.saved.saved();
    assert!(
        saved
            .executions
            .iter()
            .all(|e| e.tool_request.input["formatted"] == true)
    );
    assert!(
        matches!(&saved.nodes[&1].message.content, MessageContent::Structured(blocks)
        if blocks.iter().all(|b| matches!(b, ContentBlock::ToolUse { input, .. } if input["formatted"] == true)))
    );
}

async fn completion_is_checkpointed_while_sibling_waits(parallel: bool) {
    let requests = vec![request("one", false), request("two", true)];
    let mut f = fixture(&requests);
    if parallel {
        f.agent.hooks.dispatch = Box::new(Parallel(vec![0, 1]));
    }
    let task = tokio::spawn(async move { f.agent.manage_tool_execution(&requests).await });
    tokio::time::timeout(Duration::from_secs(2), f.entered.notified())
        .await
        .unwrap();
    let checkpointed = tokio::time::timeout(Duration::from_millis(250), async {
        loop {
            if f.saved
                .saved()
                .executions
                .iter()
                .any(|e| e.tool_request.id == "one" && e.result.is_success())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    f.release.notify_one();
    task.await.unwrap().unwrap();
    assert!(
        checkpointed,
        "a completed tool must be durable before the batch finishes"
    );
}

#[tokio::test]
async fn dispatch_sequential_completions_are_saved_individually() {
    completion_is_checkpointed_while_sibling_waits(false).await;
}

#[tokio::test]
async fn dispatch_parallel_completions_are_saved_individually() {
    completion_is_checkpointed_while_sibling_waits(true).await;
}

#[tokio::test]
async fn dispatch_journals_an_effectful_call_before_it_runs() {
    let requests = vec![request("one", true), request("two", false)];
    let f = fixture(&requests);
    let mut agent = f.agent;
    let task = tokio::spawn(async move { agent.manage_tool_execution(&requests).await });
    tokio::time::timeout(Duration::from_secs(2), f.entered.notified())
        .await
        .unwrap();
    let journal = f.saved.saved().executions;
    f.release.notify_one();
    task.await.unwrap().unwrap();

    assert_eq!(
        journal
            .iter()
            .map(|e| e.tool_request.id.as_str())
            .collect::<Vec<_>>(),
        ["one"],
        "the running call is journaled, its unstarted sibling is not"
    );
    // The record must be self-describing, even when the tool disappears.
    let restored = journal[0]
        .serialize()
        .unwrap()
        .deserialize(&ToolRegistry::new())
        .unwrap();
    assert_eq!(restored.tool_request.name, "probe");
    assert!(
        restored
            .result
            .as_render()
            .render(&mut ResourcesTracker::new())
            .contains("unknown")
    );
}

#[tokio::test]
async fn dispatch_read_only_calls_checkpoint_once() {
    let requests = vec![request("one", false)];
    let mut effectful = fixture(&requests);
    effectful
        .agent
        .manage_tool_execution(&requests)
        .await
        .unwrap();
    // Assistant message, started record, outcome, result message.
    assert_eq!(effectful.saved.saved().commits, 4);

    let mut read_only = fixture(&requests);
    read_only.agent.registry = Arc::new(probe_registry(
        &read_only,
        &["test", tools_core::spec::capabilities::READ_ONLY],
    ));
    read_only
        .agent
        .manage_tool_execution(&requests)
        .await
        .unwrap();
    // No started record: the outcome is the first thing the journal sees.
    assert_eq!(read_only.saved.saved().commits, 3);
}

struct FailingUi;

#[async_trait::async_trait]
impl AgentUi for FailingUi {
    async fn send_event(&self, event: AgentUiEvent) -> Result<(), UIError> {
        if matches!(
            event,
            AgentUiEvent::UpdateToolStatus {
                status: crate::ui::ToolStatus::Success,
                ..
            }
        ) {
            return Err(UIError::IOError(std::io::Error::other("UI disconnected")));
        }
        Ok(())
    }

    fn display_fragment(&self, _: &DisplayFragment) -> Result<(), UIError> {
        Ok(())
    }

    fn should_streaming_continue(&self) -> bool {
        true
    }

    fn notify_rate_limit(&self, _: u64) {}
    fn clear_rate_limit(&self) {}
}

#[tokio::test]
async fn dispatch_ui_failure_does_not_erase_successful_tool_evidence() {
    let requests = vec![request("one", false)];
    let mut f = fixture(&requests);
    f.agent.ui = Arc::new(FailingUi);
    let _ = f.agent.manage_tool_execution(&requests).await;
    assert!(
        f.saved
            .saved()
            .executions
            .iter()
            .any(|e| e.tool_request.id == "one" && e.result.is_success())
    );
}

#[tokio::test]
async fn dispatch_parallel_groups_do_not_cross_sequential_barriers() {
    let requests = vec![
        request("barrier", false),
        request("one", false),
        request("two", false),
    ];
    let mut f = fixture(&requests);
    f.agent.hooks.dispatch = Box::new(Parallel(vec![1, 2]));
    f.agent.manage_tool_execution(&requests).await.unwrap();
    assert_eq!(f.calls.lock().unwrap()[0], "barrier");
}

struct FailCompletionSave;

impl CheckpointPersistence for FailCompletionSave {
    fn commit(&mut self, checkpoint: &AgentCheckpoint<'_>, _: &(dyn Any + Send)) -> Result<()> {
        anyhow::ensure!(
            !checkpoint
                .changed_executions
                .iter()
                .any(|e| e.result.is_success()),
            "disk full"
        );
        Ok(())
    }
}

#[tokio::test]
async fn dispatch_checkpoint_failure_prevents_further_side_effects() {
    let requests = vec![request("one", false), request("two", false)];
    let mut f = fixture(&requests);
    f.agent.state_persistence = Box::new(FailCompletionSave);
    assert!(f.agent.manage_tool_execution(&requests).await.is_err());
    assert_eq!(
        &*f.calls.lock().unwrap(),
        &["one"],
        "persistence failure is not an ordinary tool error"
    );
}
