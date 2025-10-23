use super::*;
use crate::agent::persistence::MockStatePersistence;
use crate::session::SessionConfig;
use crate::tests::mocks::{
    create_command_executor_mock, create_test_response_text, MockLLMProvider, MockProjectManager,
    MockUI,
};
use crate::types::*;
use anyhow::Result;
use llm::types::*;
use std::path::PathBuf;
use std::sync::Arc;

/// Test basic context size calculation
#[test]
fn test_get_current_context_size() {
    let components = create_test_agent_components(vec![]);
    let mut agent = Agent::new(components, test_session_config());

    // Start with no messages - context size should be 0
    assert_eq!(agent.get_current_context_size(), 0);

    // Add a user message (no usage info)
    let user_msg = Message {
        role: MessageRole::User,
        content: MessageContent::Text("Hello".to_string()),
        request_id: None,
        usage: None,
    };
    agent.append_message(user_msg).unwrap();
    assert_eq!(agent.get_current_context_size(), 0); // Still 0 because no assistant response yet

    // Add an assistant message with usage info
    let assistant_msg = Message {
        role: MessageRole::Assistant,
        content: MessageContent::Text("Hello back".to_string()),
        request_id: Some(1),
        usage: Some(Usage {
            input_tokens: 1000,
            output_tokens: 100,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 500,
        }),
    };
    agent.append_message(assistant_msg).unwrap();

    // Should return input_tokens + cache_read_input_tokens from most recent assistant message
    assert_eq!(agent.get_current_context_size(), 1500);

    // Add another assistant message with different usage
    let assistant_msg2 = Message {
        role: MessageRole::Assistant,
        content: MessageContent::Text("More text".to_string()),
        request_id: Some(2),
        usage: Some(Usage {
            input_tokens: 2000,
            output_tokens: 200,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 1000,
        }),
    };
    agent.append_message(assistant_msg2).unwrap();

    // Should return tokens from the most recent assistant message
    assert_eq!(agent.get_current_context_size(), 3000);
}

/// Test should_compact_context logic with different configurations
#[test]
fn test_should_compact_context() {
    let components = create_test_agent_components(vec![]);
    let mut config = test_session_config();

    // Test 1: Disabled context management
    config.context_management_enabled = false;
    let mut agent = Agent::new(components, config.clone());
    agent.set_context_limit(Some(10000));

    // Add messages that would trigger compaction if enabled
    add_mock_assistant_message(&mut agent, 9000); // 90% of 10000
    assert!(!agent.should_compact_context()); // Should not compact when disabled

    // Test 2: No context limit set
    let components = create_test_agent_components(vec![]);
    config.context_management_enabled = true;
    let mut agent = Agent::new(components, config.clone());
    // Don't set context limit
    add_mock_assistant_message(&mut agent, 9000);
    assert!(!agent.should_compact_context()); // Should not compact without limit

    // Test 3: Below threshold (default 85%)
    let components = create_test_agent_components(vec![]);
    let mut agent = Agent::new(components, config.clone());
    agent.set_context_limit(Some(10000));
    add_mock_assistant_message(&mut agent, 8000); // 80% of 10000
    assert!(!agent.should_compact_context()); // Below 85% threshold

    // Test 4: At threshold
    let components = create_test_agent_components(vec![]);
    let mut agent = Agent::new(components, config.clone());
    agent.set_context_limit(Some(10000));
    add_mock_assistant_message(&mut agent, 8500); // 85% of 10000
    assert!(agent.should_compact_context()); // At 85% threshold

    // Test 5: Above threshold
    let components = create_test_agent_components(vec![]);
    let mut agent = Agent::new(components, config.clone());
    agent.set_context_limit(Some(10000));
    add_mock_assistant_message(&mut agent, 9500); // 95% of 10000
    assert!(agent.should_compact_context()); // Above 85% threshold

    // Test 6: Custom threshold
    let components = create_test_agent_components(vec![]);
    config.context_threshold = 0.75; // 75% threshold
    let mut agent = Agent::new(components, config);
    agent.set_context_limit(Some(10000));
    add_mock_assistant_message(&mut agent, 7600); // 76% of 10000
    assert!(agent.should_compact_context()); // Above custom 75% threshold
}

/// Test counting compactions
#[test]
fn test_count_compactions() {
    let components = create_test_agent_components(vec![]);
    let mut agent = Agent::new(components, test_session_config());

    assert_eq!(agent.count_compactions(), 0);

    // Add a regular message
    add_mock_user_message(&mut agent);
    assert_eq!(agent.count_compactions(), 0);

    // Add a compaction message
    add_mock_compaction_message(&mut agent, 1, "First compaction summary", 10, 5000);
    assert_eq!(agent.count_compactions(), 1);

    // Add more regular messages
    add_mock_user_message(&mut agent);
    add_mock_assistant_message(&mut agent, 1000);
    assert_eq!(agent.count_compactions(), 1);

    // Add another compaction
    add_mock_compaction_message(&mut agent, 2, "Second compaction summary", 15, 8000);
    assert_eq!(agent.count_compactions(), 2);
}

/// Test getting active messages (messages after last compaction)
#[test]
fn test_get_active_messages() {
    let components = create_test_agent_components(vec![]);
    let mut agent = Agent::new(components, test_session_config());

    // With no compaction, all messages are active
    add_mock_user_message(&mut agent);
    add_mock_assistant_message(&mut agent, 1000);
    add_mock_user_message(&mut agent);

    let active = agent.get_active_messages();
    assert_eq!(active.len(), 3);

    // Add a compaction
    add_mock_compaction_message(&mut agent, 1, "Compaction summary", 3, 1000);

    // Add more messages after compaction
    add_mock_user_message(&mut agent);
    add_mock_assistant_message(&mut agent, 1500);

    let active = agent.get_active_messages();
    // Should include: compaction message + 2 messages after it
    assert_eq!(active.len(), 3);

    // Verify the first active message is the compaction
    if let MessageContent::Structured(blocks) = &active[0].content {
        assert!(blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::ContextCompaction { .. })));
    } else {
        panic!("Expected compaction message to be first active message");
    }

    // Add another compaction
    add_mock_compaction_message(&mut agent, 2, "Second compaction", 5, 1500);
    add_mock_user_message(&mut agent);

    let active = agent.get_active_messages();
    // Should only include messages from the last compaction onwards
    assert_eq!(active.len(), 2); // Second compaction + 1 message after
}

/// Test full compaction flow
#[tokio::test]
async fn test_compact_context_flow() -> Result<()> {
    // Mock LLM to provide a summary when requested
    let mock_llm = MockLLMProvider::new(vec![Ok(create_test_response_text(concat!(
        "# Summary\n\n",
        "1. **Original Task**: User asked to implement feature X\n",
        "2. **Progress Made**: Created files A, B, C and implemented core logic\n",
        "3. **Working Memory**: Project uses Rust with async/await patterns\n",
        "4. **Next Steps**: Need to add tests and documentation"
    )))]);

    let components = AgentComponents {
        llm_provider: Box::new(mock_llm),
        project_manager: Box::new(MockProjectManager::new()),
        command_executor: Box::new(create_command_executor_mock()),
        ui: Arc::new(MockUI::default()),
        state_persistence: Box::new(MockStatePersistence::new()),
    };

    let mut agent = Agent::new(components, test_session_config());

    // Add some messages to simulate a conversation
    add_mock_user_message(&mut agent);
    add_mock_assistant_message(&mut agent, 1000);
    add_mock_user_message(&mut agent);
    add_mock_assistant_message(&mut agent, 2000);

    let messages_before = agent.get_message_history().len();
    assert_eq!(messages_before, 4);

    // Request a summary (this would normally be triggered by should_compact_context)
    let summary = agent.request_context_summary().await?;

    // Verify summary was received
    assert!(summary.contains("Original Task"));
    assert!(summary.contains("Progress Made"));

    // Compact context
    agent.compact_context(summary).await?;

    // Verify compaction message was added
    let messages_after = agent.get_message_history().len();
    assert_eq!(messages_after, messages_before + 3); // +2 for summary request/response, +1 for compaction

    // Verify the last message is a compaction message
    let last_msg = agent.get_message_history().last().unwrap();
    assert_eq!(last_msg.role, MessageRole::User);
    if let MessageContent::Structured(blocks) = &last_msg.content {
        assert!(blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::ContextCompaction { .. })));

        // Verify compaction details
        for block in blocks {
            if let ContentBlock::ContextCompaction {
                compaction_number,
                messages_archived,
                context_size_before: _,
                summary: compaction_summary,
                ..
            } = block
            {
                assert_eq!(compaction_number, &1);
                assert_eq!(messages_archived, &6); // 4 original + 2 for summary request/response
                                                   // context_size_before will be 0 because the summary response has no usage info
                                                   // This is fine for test purposes
                assert!(compaction_summary.contains("Original Task"));
            }
        }
    } else {
        panic!("Expected compaction message to have structured content");
    }

    // Verify active messages only include compaction and newer
    let active = agent.get_active_messages();
    assert_eq!(active.len(), 1); // Only the compaction message

    Ok(())
}

/// Test multiple compactions in sequence
#[tokio::test]
async fn test_multiple_compactions() -> Result<()> {
    let mock_llm = MockLLMProvider::new(vec![
        Ok(create_test_response_text("Second compaction summary")),
        Ok(create_test_response_text("First compaction summary")),
    ]);

    let components = AgentComponents {
        llm_provider: Box::new(mock_llm),
        project_manager: Box::new(MockProjectManager::new()),
        command_executor: Box::new(create_command_executor_mock()),
        ui: Arc::new(MockUI::default()),
        state_persistence: Box::new(MockStatePersistence::new()),
    };

    let mut agent = Agent::new(components, test_session_config());

    // First compaction
    add_mock_user_message(&mut agent);
    add_mock_assistant_message(&mut agent, 1000);
    let summary1 = agent.request_context_summary().await?;
    agent.compact_context(summary1).await?;

    assert_eq!(agent.count_compactions(), 1);
    let active_after_first = agent.get_active_messages();
    assert_eq!(active_after_first.len(), 1);

    // Add more messages
    add_mock_user_message(&mut agent);
    add_mock_assistant_message(&mut agent, 2000);
    add_mock_user_message(&mut agent);

    // Second compaction
    let summary2 = agent.request_context_summary().await?;
    agent.compact_context(summary2).await?;

    assert_eq!(agent.count_compactions(), 2);

    // Should only have messages from the second compaction onwards
    let active_after_second = agent.get_active_messages();
    assert_eq!(active_after_second.len(), 1); // Just the second compaction message

    // Verify the second compaction has correct metadata
    if let MessageContent::Structured(blocks) = &active_after_second[0].content {
        for block in blocks {
            if let ContentBlock::ContextCompaction {
                compaction_number, ..
            } = block
            {
                assert_eq!(compaction_number, &2);
            }
        }
    }

    Ok(())
}

/// Test that compaction is serializable/deserializable
#[test]
fn test_compaction_serialization() -> Result<()> {
    let compaction_block = ContentBlock::new_context_compaction(
        1,
        "Test summary of the conversation".to_string(),
        45,
        150000,
    );

    // Serialize
    let json = serde_json::to_string(&compaction_block)?;
    assert!(json.contains("context_compaction"));
    assert!(json.contains("Test summary"));

    // Deserialize
    let deserialized: ContentBlock = serde_json::from_str(&json)?;

    // Verify equality (ignoring timestamps)
    assert!(compaction_block.eq_ignore_timestamps(&deserialized));

    if let ContentBlock::ContextCompaction {
        compaction_number,
        summary,
        messages_archived,
        context_size_before,
        ..
    } = deserialized
    {
        assert_eq!(compaction_number, 1);
        assert_eq!(summary, "Test summary of the conversation");
        assert_eq!(messages_archived, 45);
        assert_eq!(context_size_before, 150000);
    } else {
        panic!("Failed to deserialize as ContextCompaction");
    }

    Ok(())
}

/// Test the summary request message generation
#[test]
fn test_generate_summary_request() {
    let components = create_test_agent_components(vec![]);
    let agent = Agent::new(components, test_session_config());

    let request_text = agent.generate_summary_request();

    assert!(request_text.contains("<system-context-management>"));
    assert!(request_text.contains("context window is approaching its limit"));
    assert!(request_text.contains("Original Task"));
    assert!(request_text.contains("Progress Made"));
    assert!(request_text.contains("Working Memory"));
    assert!(request_text.contains("Next Steps"));
    assert!(request_text.contains("Do NOT use tools"));
}

/// Test that context_config is properly initialized from SessionConfig
#[test]
fn test_context_config_initialization() {
    let components = create_test_agent_components(vec![]);

    // Test default configuration
    let default_config = test_session_config();
    let agent = Agent::new(components, default_config);

    // Check that context config was initialized correctly
    let context_config = agent.get_context_config();
    assert!(context_config.enabled);
    assert_eq!(context_config.threshold, 0.85);
    assert!(context_config.limit.is_none()); // No limit until set

    // Test custom configuration
    let components = create_test_agent_components(vec![]);
    let mut custom_config = test_session_config();
    custom_config.context_threshold = 0.75;
    custom_config.context_management_enabled = false;

    let agent = Agent::new(components, custom_config);
    let context_config = agent.get_context_config();
    assert!(!context_config.enabled);
    assert_eq!(context_config.threshold, 0.75);
}

/// Test setting context limit
#[test]
fn test_set_context_limit() {
    let components = create_test_agent_components(vec![]);
    let mut agent = Agent::new(components, test_session_config());

    // Initially no limit
    assert!(agent.get_context_config().limit.is_none());

    // Set a limit
    agent.set_context_limit(Some(100000));
    assert_eq!(agent.get_context_config().limit, Some(100000));

    // Clear the limit
    agent.set_context_limit(None);
    assert!(agent.get_context_config().limit.is_none());
}

// Helper functions

fn create_test_agent_components(responses: Vec<Result<LLMResponse>>) -> AgentComponents {
    AgentComponents {
        llm_provider: Box::new(MockLLMProvider::new(responses)),
        project_manager: Box::new(MockProjectManager::new()),
        command_executor: Box::new(create_command_executor_mock()),
        ui: Arc::new(MockUI::default()),
        state_persistence: Box::new(MockStatePersistence::new()),
    }
}

fn test_session_config() -> SessionConfig {
    SessionConfig {
        init_path: Some(PathBuf::from("./test_path")),
        initial_project: String::new(),
        tool_syntax: ToolSyntax::Native,
        use_diff_blocks: false,
        context_threshold: 0.85,
        context_management_enabled: true,
        ..SessionConfig::default()
    }
}

fn add_mock_user_message(agent: &mut Agent) {
    let msg = Message {
        role: MessageRole::User,
        content: MessageContent::Text("Test user message".to_string()),
        request_id: None,
        usage: None,
    };
    agent.append_message(msg).unwrap();
}

fn add_mock_assistant_message(agent: &mut Agent, input_tokens: u32) {
    let msg = Message {
        role: MessageRole::Assistant,
        content: MessageContent::Text("Test assistant message".to_string()),
        request_id: Some(agent.get_next_request_id()),
        usage: Some(Usage {
            input_tokens,
            output_tokens: 100,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        }),
    };
    agent.append_message(msg).unwrap();
}

fn add_mock_compaction_message(
    agent: &mut Agent,
    compaction_number: u32,
    summary: &str,
    messages_archived: usize,
    context_size_before: u32,
) {
    let compaction_block = ContentBlock::new_context_compaction(
        compaction_number,
        summary.to_string(),
        messages_archived,
        context_size_before,
    );

    let msg = Message {
        role: MessageRole::User,
        content: MessageContent::Structured(vec![
            compaction_block,
            ContentBlock::new_text("Context has been compacted. Continue based on the summary."),
        ]),
        request_id: None,
        usage: None,
    };
    agent.append_message(msg).unwrap();
}
