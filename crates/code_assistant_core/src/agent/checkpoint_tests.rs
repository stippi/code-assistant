use super::*;
use crate::agent::persistence::AgentStatePersistence;
use crate::persistence::{ChatSession, FileSessionPersistence, MessageNode};
use std::sync::Mutex;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Option<SessionState>>>);
impl AgentStatePersistence for Capture {
    fn save_agent_state(&mut self, state: SessionState) -> Result<()> {
        *self.0.lock().unwrap() = Some(state);
        Ok(())
    }
}

/// Deterministic format-on-save without depending on an installed formatter.
struct FormattingTool;
#[async_trait::async_trait]
impl tools_core::Tool for FormattingTool {
    type Input = serde_json::Value;
    type Output = agent_core::types::ParseError;
    fn spec(&self) -> tools_core::ToolSpec {
        crate::tools::test_registry()
            .get("write_file")
            .unwrap()
            .spec()
    }
    async fn execute<'a>(
        &self,
        _: &mut tools_core::ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        input["content"] = "formatted content\n".into();
        Ok(agent_core::types::ParseError::new("test output".into()))
    }
}

async fn formatted_roundtrip(syntax: ToolSyntax) -> Result<()> {
    let dir = tempdir()?;
    let mut registry = tools_core::ToolRegistry::new();
    registry.register(Box::new(FormattingTool));
    let registry = Arc::new(registry);
    let dialect = crate::tool_dialects::dialect_for(syntax);
    let request = crate::tools::ToolRequest {
        id: "call".into(),
        name: "write_file".into(),
        input: serde_json::json!({"project":"test", "path":"test.txt", "content":"unformatted"}),
        start_offset: None,
        end_offset: None,
    };
    let content = if syntax == ToolSyntax::Native {
        vec![ContentBlock::new_tool_use(
            &request.id,
            &request.name,
            request.input.clone(),
        )]
    } else {
        vec![
            // Offsets belong to the tool-containing block, not this preamble.
            ContentBlock::new_text("Preamble remains intact.\n"),
            ContentBlock::new_text(format!(
                "{}\nTRAILING TEXT MUST BE TRUNCATED",
                dialect.format_tool_request(&request, &registry)?
            )),
        ]
    };
    let mock_llm = MockLLMProvider::new(vec![
        Ok(create_test_response_text("done")),
        Ok(LLMResponse {
            content,
            usage: Usage::zero(),
            rate_limit_info: None,
        }),
    ]);
    let captured = Capture::default();
    let components = AgentComponents {
        llm_provider: Box::new(mock_llm),
        project_manager: Arc::new(MockProjectManager::new()),
        command_executor: Arc::new(create_command_executor_mock()),
        ui: Arc::new(MockUI::default()),
        state_persistence: Box::new(captured.clone()),
        permission_handler: None,
        permissions: Default::default(),
        tool_registry: registry.clone(),
        sub_agent_runner: None,
        wakeups: None,
        pty_sessions: None,
        browser_sessions: None,
        terminal_interrupts: None,
        session_source: None,
        hooks_factory: None,
    };
    let config = SessionConfig {
        tool_syntax: syntax,
        ..Default::default()
    };
    let mut agent = Agent::new(components, config.clone());
    let mut initial = SessionState::from_messages(
        "checkpoint",
        "test",
        vec![Message::new_user("write")],
        config.clone(),
    );
    initial.message_nodes.insert(
        99,
        MessageNode {
            id: 99,
            message: Message::new_assistant("inactive branch"),
            parent_id: Some(1),
            created_at: std::time::SystemTime::now(),
            extension: Some(serde_json::json!({"branch-data": true})),
        },
    );
    initial.next_node_id = 100;
    agent.load_from_session_state(initial).await?;
    agent.run_single_iteration().await?;
    let state = captured.0.lock().unwrap().take().unwrap();
    let mut session = ChatSession::new_empty("checkpoint".into(), "test".into(), config, None);
    session.message_nodes = state.message_nodes;
    session.active_path = state.active_path;
    session.next_node_id = state.next_node_id;
    session.tool_executions = state
        .tool_executions
        .iter()
        .map(|e| e.serialize())
        .collect::<Result<_>>()?;
    let mut persistence = FileSessionPersistence::new_with_root_dir(dir.path().to_path_buf());
    persistence.save_chat_session(&session)?;
    let loaded = persistence.load_chat_session("checkpoint")?.unwrap();
    assert_eq!(
        loaded.message_nodes[&99].extension,
        Some(serde_json::json!({"branch-data": true}))
    );
    assert!(!loaded.active_path.contains(&99));
    let canonical = loaded.get_active_messages_cloned();
    assert_eq!(
        serde_json::to_value(&canonical)?,
        serde_json::to_value(&state.messages)?
    );
    let tool_message = &canonical[1];
    let MessageContent::Structured(blocks) = &tool_message.content else {
        panic!("tool message")
    };
    let response = LLMResponse {
        content: blocks.clone(),
        usage: Usage::zero(),
        rate_limit_info: None,
    };
    let (parsed, _) =
        dialect.extract_requests(&response, tool_message.request_id.unwrap(), 0, &registry)?;
    assert_eq!(parsed.len(), 1);
    // XML's multiline delimiters include surrounding newlines in parsed input.
    assert_eq!(
        parsed[0].input["content"].as_str().unwrap().trim(),
        "formatted content"
    );
    let serialized = serde_json::to_string(tool_message)?;
    if syntax != ToolSyntax::Native {
        assert!(serialized.contains("Preamble remains intact."));
        assert!(!serialized.contains("TRAILING TEXT MUST BE TRUNCATED"));
    }
    Ok(())
}

#[test]
fn journal_outcomes_survive_disk_reload_without_the_original_tools() -> Result<()> {
    use agent_core::execution::{ExecutionState, RuntimeToolOutput};
    let dir = tempdir()?;
    let mut session = ChatSession::new_empty(
        "journal".into(),
        "test".into(),
        SessionConfig::default(),
        None,
    );
    let outputs = [
        RuntimeToolOutput {
            state: ExecutionState::Failed,
            message: "failed before interruption".into(),
        },
        RuntimeToolOutput::started(),
        RuntimeToolOutput::not_started("No invocation was made."),
    ];
    for (i, output) in outputs.into_iter().enumerate() {
        let id = format!("call-{i}");
        let request = agent_core::ToolRequest {
            id: id.clone(),
            name: "unavailable-tool".into(),
            input: serde_json::json!({"path":"evidence.txt"}),
            start_offset: None,
            end_offset: None,
        };
        session.add_message(Message::new_assistant_content(vec![
            ContentBlock::new_tool_use(&id, &request.name, request.input.clone()),
        ]));
        session.tool_executions.push(
            agent_core::ToolExecution {
                tool_request: request,
                result: Box::new(output),
            }
            .serialize()?,
        );
    }
    let mut store = FileSessionPersistence::new_with_root_dir(dir.path().to_path_buf());
    store.save_chat_session(&session)?;
    let loaded = store.load_chat_session("journal")?.unwrap();
    let registry = tools_core::ToolRegistry::new();
    let restored: Vec<_> = loaded
        .tool_executions
        .iter()
        .map(|entry| {
            assert!(crate::tools::mcp::execution_renderable(entry, &registry));
            crate::tools::mcp::deserialize_tool_execution(entry, &registry)
        })
        .collect::<Result<_>>()?;
    assert_eq!(restored.len(), 3);
    for entry in &restored {
        assert_eq!(entry.tool_request.name, "unavailable-tool");
        assert_eq!(entry.tool_request.input["path"], "evidence.txt");
    }
    let mut tracker = tools_core::ResourcesTracker::new();
    assert!(
        restored[0]
            .result
            .as_render()
            .render(&mut tracker)
            .contains("failed before interruption")
    );
    assert!(
        restored[1]
            .result
            .as_render()
            .render(&mut tracker)
            .contains("unknown")
    );
    assert!(
        restored[2]
            .result
            .as_render()
            .render(&mut tracker)
            .contains("not started")
    );
    Ok(())
}

#[tokio::test]
async fn checkpoint_native_formatted_roundtrip() -> Result<()> {
    formatted_roundtrip(ToolSyntax::Native).await
}
#[tokio::test]
async fn checkpoint_xml_formatted_roundtrip() -> Result<()> {
    formatted_roundtrip(ToolSyntax::Xml).await
}
#[tokio::test]
async fn checkpoint_caret_formatted_roundtrip() -> Result<()> {
    formatted_roundtrip(ToolSyntax::Caret).await
}
