mod dispatch_tests;

use super::*;
use crate::hooks::*;
use serde_json::json;

/// Inert implementation of every collaborator the runtime needs.
struct Stub;

#[async_trait::async_trait]
impl LLMProvider for Stub {
    async fn send_message(
        &mut self,
        _: LLMRequest,
        _: Option<&StreamingCallback>,
    ) -> Result<llm::LLMResponse> {
        anyhow::bail!("unexpected LLM call")
    }
}

#[async_trait::async_trait]
impl AgentUi for Stub {
    async fn send_event(&self, _: AgentUiEvent) -> Result<(), UIError> {
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

impl ToolServicesProvider for Stub {
    fn begin(&self, _: &mut (dyn Any + Send), _: &str) -> Box<dyn Any + Send> {
        Box::new(())
    }

    fn end(&self, _: &mut (dyn Any + Send), _: Box<dyn Any + Send>) {}

    fn detached(&self, _: &str) -> Box<dyn Any + Send> {
        Box::new(())
    }
}

impl ToolDispatchPolicy for Stub {
    fn parallel_indices(&self, _: &[ToolRequest]) -> Vec<usize> {
        vec![]
    }
}

impl CompactionPolicy for Stub {
    fn context_limit(&self, _: &(dyn Any + Send)) -> Result<Option<u32>> {
        Ok(None)
    }

    fn should_compact(&self, _: &ContextSnapshot) -> bool {
        false
    }

    fn compaction_prompt(&self) -> &str {
        "summarize"
    }
}

impl RecoveryPolicy for Stub {
    fn classify(&self, _: &anyhow::Error, _: u32) -> RecoveryAction {
        RecoveryAction::Fail
    }
}

impl SystemPromptProvider for Stub {
    fn build(&self, _: &PromptCtx) -> String {
        String::new()
    }
}

/// What a session store would hold after merging every checkpoint.
#[derive(Clone, Default)]
struct Saved {
    nodes: BTreeMap<NodeId, MessageNode>,
    active_path: ConversationPath,
    next_node_id: NodeId,
    executions: Vec<ToolExecution>,
    /// Sizes of the most recent checkpoint: changed nodes, changed executions.
    last_delta: (usize, usize),
    commits: usize,
}

/// Merges checkpoints the way a session store does.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Saved>>);

impl Capture {
    fn saved(&self) -> Saved {
        self.0.lock().unwrap().clone()
    }
}

impl CheckpointPersistence for Capture {
    fn commit(&mut self, checkpoint: &AgentCheckpoint<'_>, _: &(dyn Any + Send)) -> Result<()> {
        let mut saved = self.0.lock().unwrap();
        for node in &checkpoint.changed_nodes {
            saved.nodes.insert(node.id, (*node).clone());
        }
        saved.active_path = checkpoint.active_path.to_vec();
        saved.next_node_id = checkpoint.next_node_id;
        for execution in &checkpoint.changed_executions {
            let execution = execution.try_clone()?;
            let id = &execution.tool_request.id;
            match saved
                .executions
                .iter()
                .position(|entry| &entry.tool_request.id == id)
            {
                Some(index) => saved.executions[index] = execution,
                None => saved.executions.push(execution),
            }
        }
        saved.last_delta = (
            checkpoint.changed_nodes.len(),
            checkpoint.changed_executions.len(),
        );
        saved.commits += 1;
        Ok(())
    }
}

fn runtime() -> (AgentRuntime, Capture) {
    let capture = Capture::default();
    let mut runtime = AgentRuntime::new(AgentRuntimeComponents {
        llm_provider: Box::new(Stub),
        dialect: Arc::new(crate::native::NativeDialect),
        ui: Arc::new(Stub),
        registry: Arc::new(ToolRegistry::new()),
        tool_capability: String::new(),
        excluded_tool_capabilities: vec![],
        stream_hidden_tools: Arc::new(|_| false),
        command_executor: Arc::new(command_executor::DefaultCommandExecutor),
        permission_handler: None,
        permissions: Default::default(),
        services_provider: Arc::new(Stub),
        state_persistence: Box::new(capture.clone()),
        hooks: HookRegistry {
            interceptors: vec![],
            iteration_hooks: vec![],
            observers: vec![],
            dispatch: Box::new(Stub),
            compaction: Box::new(Stub),
            recovery: Box::new(Stub),
            system_prompt: Box::new(Stub),
        },
        extensions: Box::new(()),
    });
    runtime.set_session_id(Some("test-session".into()));
    (runtime, capture)
}

/// A runtime restored from what the store holds, through the serialized
/// tree rather than an in-memory alias of its messages.
fn reload(saved: Saved) -> AgentRuntime {
    let (mut restored, _) = runtime();
    let nodes = serde_json::from_value(serde_json::to_value(saved.nodes).unwrap()).unwrap();
    restored.restore_conversation(nodes, saved.active_path, saved.next_node_id, Vec::new());
    restored.set_tool_executions(saved.executions);
    restored
}

fn call(id: &str) -> ContentBlock {
    ContentBlock::new_tool_use(id, "write_file", json!({"content": "unformatted"}))
}

fn result(id: &str) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: id.into(),
        content: ToolResultContent::text("evidence"),
        is_error: None,
        start_time: None,
        end_time: None,
    }
}

fn text(message: &Message) -> &str {
    match &message.content {
        MessageContent::Text(text) => text,
        MessageContent::Structured(_) => panic!("expected a text message"),
    }
}

#[test]
fn checkpoint_carries_only_the_changes_since_the_previous_one() {
    let (mut agent, saved) = runtime();
    agent.append_message(Message::new_user("one")).unwrap();
    agent.append_message(Message::new_assistant("two")).unwrap();
    assert_eq!(saved.saved().last_delta, (1, 0));

    agent
        .journal
        .record(ToolExecution::create_parse_error("a".into(), "x".into()));
    agent.checkpoint().unwrap();
    assert_eq!(saved.saved().last_delta, (0, 1));

    agent.checkpoint().unwrap();
    let state = saved.saved();
    assert_eq!(state.last_delta, (0, 0));
    assert_eq!(state.nodes.len(), 2);
    assert_eq!(state.executions.len(), 1);
}

#[test]
fn checkpoint_failure_keeps_the_changes_for_the_next_attempt() {
    struct FailOnce(Capture, bool);

    impl CheckpointPersistence for FailOnce {
        fn commit(
            &mut self,
            checkpoint: &AgentCheckpoint<'_>,
            extensions: &(dyn Any + Send),
        ) -> Result<()> {
            if std::mem::replace(&mut self.1, false) {
                anyhow::bail!("disk full");
            }
            self.0.commit(checkpoint, extensions)
        }
    }

    let (mut agent, saved) = runtime();
    agent.state_persistence = Box::new(FailOnce(saved.clone(), true));
    assert!(agent.append_message(Message::new_user("one")).is_err());
    agent.append_message(Message::new_assistant("two")).unwrap();
    let state = saved.saved();
    assert_eq!(state.commits, 1);
    assert_eq!(state.last_delta, (2, 0));
    assert_eq!(state.nodes.len(), 2);
}

#[test]
fn checkpoint_legacy_history_is_imported_only_without_a_tree() {
    let (mut agent, saved) = runtime();
    agent.restore_conversation(
        BTreeMap::new(),
        Vec::new(),
        1,
        vec![Message::new_user("legacy")],
    );
    agent.append_message(Message::new_assistant("new")).unwrap();
    let mut state = saved.saved();
    assert_eq!(state.nodes.len(), 2);
    assert_eq!(state.nodes[&2].parent_id, Some(1));

    // A nonempty tree with an intentionally empty active path is authoritative
    // too: neither reactivate a branch nor import stale linear messages.
    state.active_path.clear();
    let restored = reload(state);
    assert!(restored.message_history().is_empty());
    assert_eq!(restored.conversation.nodes().len(), 2);
}

#[test]
fn checkpoint_persists_hook_edits_to_the_tree() {
    struct Correction;

    impl ToolInterceptor for Correction {
        fn try_intercept(
            &self,
            _: &ToolRequest,
            ctx: &mut LoopCtx,
        ) -> Option<Result<Box<dyn tools_core::AnyOutput>>> {
            ctx.conversation.node_mut(1).unwrap().message = Message::new_user("corrected by hook");
            Some(Ok(Box::new(crate::types::ParseError::new(
                "handled".into(),
            ))))
        }
    }

    let (mut agent, saved) = runtime();
    agent.append_message(Message::new_user("before")).unwrap();
    agent.hooks.interceptors.push(Box::new(Correction));
    assert!(
        agent
            .intercept_tool(&ToolRequest::from(&call("a")))
            .unwrap()
            .is_ok()
    );
    agent.checkpoint().unwrap();
    let state = saved.saved();
    assert_eq!(state.last_delta, (1, 0));
    assert_eq!(text(&state.nodes[&1].message), "corrected by hook");
    assert_eq!(text(&agent.message_history()[0]), "corrected by hook");
}

#[test]
fn checkpoint_formatted_input_survives_roundtrip() {
    let (mut agent, saved) = runtime();
    agent
        .append_message(Message::new_assistant_content(vec![call("a")]))
        .unwrap();
    let mut request = ToolRequest::from(&call("a"));
    request.input = json!({"content": "formatted"});
    agent.update_message_history_with_formatted_tool(&request);
    agent
        .append_message(Message::new_user_content(vec![result("a")]))
        .unwrap();

    let restored = reload(saved.saved());
    assert!(matches!(&restored.message_history()[0].content,
        MessageContent::Structured(blocks) if matches!(&blocks[0], ContentBlock::ToolUse { input, .. } if input == &request.input)));
}

#[test]
fn checkpoint_dangling_calls_without_a_record_render_as_not_run() {
    let (mut agent, saved) = runtime();
    agent
        .append_message(Message::new_assistant_content(vec![call("a"), call("b")]))
        .unwrap();
    // Partial result: the missing result must be merged into this prompt message.
    agent
        .append_message(Message::new_user_content(vec![result("a")]))
        .unwrap();

    let restored = reload(saved.saved());
    let before = serde_json::to_value(restored.message_history()).unwrap();
    let prompt = restored.render_tool_results_in_messages();
    let MessageContent::Structured(blocks) = &prompt[1].content else {
        panic!("results")
    };
    assert_eq!(
        blocks.len(),
        2,
        "partial tool results must be repaired by id"
    );
    assert!(blocks.iter().any(|block| matches!(block, ContentBlock::ToolResult { tool_use_id, content, .. }
        if tool_use_id == "b" && content.contains("did not run") && !content.contains("cancelled by user"))));
    assert_eq!(
        before,
        serde_json::to_value(restored.message_history()).unwrap()
    );
}

#[test]
fn checkpoint_dangling_tail_is_not_deleted() {
    let (mut agent, saved) = runtime();
    agent.append_message(Message::new_user("task")).unwrap();
    agent
        .append_message(Message::new_assistant_content(vec![call("a")]))
        .unwrap();

    let restored = reload(saved.saved());
    assert_eq!(restored.message_history().len(), 2);
    assert_eq!(restored.render_tool_results_in_messages().len(), 3);
}

#[test]
fn checkpoint_recovery_keeps_canonical_messages_and_tool_evidence() {
    let (mut agent, saved) = runtime();
    agent
        .append_message(Message::new_assistant_content(vec![call("a")]))
        .unwrap();
    agent
        .append_message(Message::new_user_content(vec![result("a")]))
        .unwrap();
    // A serializable large output suffices to exercise size-based recovery.
    agent.journal.record(ToolExecution::create_parse_error(
        "a".into(),
        "x".repeat(60 * 1024),
    ));
    agent.checkpoint().unwrap();
    let before = serde_json::to_value(agent.message_history()).unwrap();
    let evidence = agent.journal.entries()[0].serialize().unwrap();

    assert_eq!(agent.replace_large_tool_results().len(), 1);
    let projected = agent.render_tool_results_in_messages();
    assert!(serde_json::to_string(&projected).unwrap().len() < 10 * 1024);
    // XML/caret conversion must not re-render the original large execution.
    let text_projection = agent.convert_tool_results_to_text(projected);
    assert!(serde_json::to_string(&text_projection).unwrap().len() < 10 * 1024);
    assert!(agent.replace_large_tool_results().is_empty());
    assert_eq!(
        before,
        serde_json::to_value(agent.message_history()).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&evidence).unwrap(),
        serde_json::to_value(agent.journal.entries()[0].serialize().unwrap()).unwrap()
    );

    agent.drop_last_tool_exchange();
    assert!(agent.render_tool_results_in_messages().is_empty());
    agent.checkpoint().unwrap();
    let restored = reload(saved.saved());
    assert_eq!(
        before,
        serde_json::to_value(restored.message_history()).unwrap()
    );
    assert_eq!(restored.journal.entries().len(), 1);
    assert!(
        serde_json::to_string(&restored.render_tool_results_in_messages())
            .unwrap()
            .len()
            > 50 * 1024
    );
}
