use super::*;
use crate::hooks::*;
use serde_json::json;

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
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Option<AgentSnapshot>>>);
impl SnapshotPersistence for Capture {
    fn save(&mut self, snapshot: AgentSnapshot, _: &(dyn Any + Send)) -> Result<()> {
        *self.0.lock().unwrap() = Some(snapshot);
        Ok(())
    }
}
fn runtime() -> (AgentRuntime, Capture) {
    let capture = Capture::default();
    let runtime = AgentRuntime::new(AgentRuntimeComponents {
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
    (runtime, capture)
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
    let mut snapshot = saved.0.lock().unwrap().take().unwrap();
    assert_eq!(snapshot.message_nodes.len(), 2);
    assert_eq!(snapshot.message_nodes[&2].parent_id, Some(1));
    // A nonempty tree with an intentionally empty active path is authoritative
    // too: neither reactivate a branch nor import stale linear messages.
    snapshot.active_path.clear();
    let restored = reload(snapshot);
    assert!(restored.message_history().is_empty());
    assert_eq!(restored.conversation.nodes().len(), 2);
}

#[test]
fn checkpoint_hook_message_corrections_rebuild_cache_even_on_early_return() {
    struct Correction;
    impl ToolInterceptor for Correction {
        fn try_intercept(&self, _: &ToolRequest, ctx: &mut LoopCtx) -> Option<Result<bool>> {
            ctx.message_nodes.get_mut(&1).unwrap().message = Message::new_user("corrected by hook");
            Some(Ok(true))
        }
    }
    let (mut agent, saved) = runtime();
    agent.append_message(Message::new_user("before")).unwrap();
    agent.hooks.interceptors.push(Box::new(Correction));
    assert!(
        agent
            .intercept_tool(&ToolRequest::from(&call("a")))
            .unwrap()
            .unwrap()
    );
    agent.save_state().unwrap();
    let snapshot = saved.0.lock().unwrap().take().unwrap();
    assert_eq!(
        serde_json::to_value(&snapshot.message_nodes[&1].message).unwrap(),
        serde_json::to_value(&snapshot.messages[0]).unwrap()
    );
    assert!(
        matches!(&snapshot.messages[0].content, MessageContent::Text(text) if text == "corrected by hook")
    );
}

fn reload(snapshot: AgentSnapshot) -> AgentRuntime {
    let (mut restored, _) = runtime();
    // Exercise the serialized tree, not an in-memory alias of its messages.
    let nodes =
        serde_json::from_value(serde_json::to_value(snapshot.message_nodes).unwrap()).unwrap();
    restored.restore_conversation(
        nodes,
        snapshot.active_path,
        snapshot.next_node_id,
        snapshot.messages,
    );
    restored.set_tool_executions(snapshot.tool_executions);
    restored.normalize_loaded_message_history();
    restored
}

#[test]
fn checkpoint_tree_wins_over_stale_linear_history() {
    let (mut agent, saved) = runtime();
    agent
        .append_message(Message::new_user("canonical"))
        .unwrap();
    let mut snapshot = saved.0.lock().unwrap().take().unwrap();
    snapshot.messages = vec![Message::new_user("stale")];
    let restored = reload(snapshot);
    assert!(
        matches!(&restored.message_history()[0].content, MessageContent::Text(text) if text == "canonical")
    );
}

#[test]
fn checkpoint_formatted_input_survives_roundtrip() {
    let (mut agent, saved) = runtime();
    agent
        .append_message(Message::new_assistant_content(vec![call("a")]))
        .unwrap();
    let mut request = ToolRequest::from(&call("a"));
    request.input = json!({"content": "formatted"});
    agent
        .update_message_history_with_formatted_tool(&request)
        .unwrap();
    agent
        .append_message(Message::new_user_content(vec![result("a")]))
        .unwrap();
    let snapshot = saved.0.lock().unwrap().take().unwrap();
    assert_eq!(
        serde_json::to_value(&snapshot.message_nodes[&1].message.content).unwrap(),
        serde_json::to_value(&snapshot.messages[0].content).unwrap()
    );
    let restored = reload(snapshot);
    assert!(matches!(&restored.message_history()[0].content,
        MessageContent::Structured(blocks) if matches!(&blocks[0], ContentBlock::ToolUse { input, .. } if input == &request.input)));
}

#[test]
fn checkpoint_dangling_calls_survive_reload_with_unknown_prompt_outcome() {
    let (mut agent, saved) = runtime();
    agent
        .append_message(Message::new_assistant_content(vec![call("a"), call("b")]))
        .unwrap();
    // Partial result: the missing result must be merged into this prompt message.
    agent
        .append_message(Message::new_user_content(vec![result("a")]))
        .unwrap();
    let mut restored = reload(saved.0.lock().unwrap().take().unwrap());
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
        if tool_use_id == "b" && content.contains("unknown") && !content.contains("cancelled by user"))));
    restored.normalize_loaded_message_history();
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
    let restored = reload(saved.0.lock().unwrap().take().unwrap());
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
    agent.set_tool_executions(vec![ToolExecution::create_parse_error(
        "a".into(),
        "x".repeat(60 * 1024),
    )]);
    let before = serde_json::to_value(agent.message_history()).unwrap();
    let evidence = agent.tool_executions[0].serialize().unwrap();
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
        serde_json::to_value(agent.tool_executions[0].serialize().unwrap()).unwrap()
    );
    agent.drop_last_tool_exchange();
    assert!(agent.render_tool_results_in_messages().is_empty());
    agent.save_state().unwrap();
    let restored = reload(saved.0.lock().unwrap().take().unwrap());
    assert_eq!(
        before,
        serde_json::to_value(restored.message_history()).unwrap()
    );
    assert_eq!(restored.tool_executions.len(), 1);
    assert!(
        serde_json::to_string(&restored.render_tool_results_in_messages())
            .unwrap()
            .len()
            > 50 * 1024
    );
}
