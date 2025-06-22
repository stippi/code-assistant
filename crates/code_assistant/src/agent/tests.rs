use super::*;
use crate::agent::persistence::MockStatePersistence;
use crate::agent::runner::parse_llm_response;
use crate::tests::mocks::MockLLMProvider;
use crate::tests::mocks::{
    create_command_executor_mock, create_test_response, create_test_response_text,
    MockProjectManager, MockUI,
};
use crate::types::*;
use crate::UserInterface;
use anyhow::Result;
use llm::types::*;
use std::path::PathBuf;
use std::sync::Arc;

#[test]
fn test_flexible_xml_parsing() -> Result<()> {
    let text = concat!(
        "I will search for TODO comments in the code.\n",
        "\n",
        "<tool:search_files>\n",
        "<param:project>test</param:project>\n",
        "<param:regex>TODO & FIXME <html></param:regex>\n",
        "</tool:search_files>"
    )
    .to_string();
    let response = LLMResponse {
        content: vec![ContentBlock::Text { text }],
        usage: Usage::zero(),
    };

    // Use a test request_id
    let request_id = 42;

    let tool_requests = parse_llm_response(&response, request_id)?;
    assert_eq!(tool_requests.len(), 1);

    let request = &tool_requests[0];
    assert_eq!(request.name, "search_files");
    if let Some(regex) = request.input.get("regex") {
        assert_eq!(regex.as_str().unwrap(), "TODO & FIXME <html>"); // Notice the & character is allowed and also tags
    } else {
        panic!("Missing regex parameter");
    }

    Ok(())
}

#[test]
fn test_replacement_xml_parsing() -> Result<()> {
    let text = concat!(
        "I will fix the code formatting.\n",
        "\n",
        "<tool:replace_in_file>\n",
        "<param:project>test</param:project>\n",
        "<param:path>src/main.rs</param:path>\n",
        "<param:diff>\n",
        "<<<<<<< SEARCH\n",
        "function test(){\n",
        "  console.log(\"messy\");\n",
        "}\n",
        "=======\n",
        "function test() {\n",
        "    console.log(\"clean\");\n",
        "}\n",
        ">>>>>>> REPLACE\n",
        "\n",
        "<<<<<<< SEARCH\n",
        "const x=42\n",
        "=======\n",
        "const x = 42;\n",
        ">>>>>>> REPLACE\n",
        "</param:diff>\n",
        "</tool:replace_in_file>\n",
    )
    .to_string();
    let response = LLMResponse {
        content: vec![ContentBlock::Text { text }],
        usage: Usage::zero(),
    };

    // Use a test request_id
    let request_id = 42;
    let tool_requests = parse_llm_response(&response, request_id)?;
    assert_eq!(tool_requests.len(), 1);

    let request = &tool_requests[0];
    assert_eq!(request.name, "replace_in_file");
    assert_eq!(
        request.input.get("project").unwrap().as_str().unwrap(),
        "test"
    );
    assert_eq!(
        request.input.get("path").unwrap().as_str().unwrap(),
        "src/main.rs"
    );

    let diff = request.input.get("diff").unwrap().as_str().unwrap();
    assert!(diff.contains("<<<<<<< SEARCH"));
    assert!(diff.contains("function test(){"));
    assert!(diff.contains("console.log(\"messy\")"));
    assert!(diff.contains("function test() {"));
    assert!(diff.contains("console.log(\"clean\")"));
    assert!(diff.contains("const x=42"));
    assert!(diff.contains("const x = 42;"));

    Ok(())
}

#[tokio::test]
async fn test_mixed_tool_start_end() -> Result<()> {
    let text = concat!(
        "Now I will take a look at the drop down implementation:\n",
        "\n",
        "<tool:read_files>\n",
        "<param:project>gpui-component</param:project>\n",
        "<param:path>crates/ui/src/dropdown.rs</param:path>\n",
        "<param:path>crates/ui/src/menu</param:path>\n",
        "</tool:list_files>"
    )
    .to_string();
    let response = LLMResponse {
        content: vec![ContentBlock::Text { text }],
        usage: Usage::zero(),
    };

    let result = parse_llm_response(&response, 1);
    println!("result: {:?}", result);

    // This should return an error, not Ok([])
    assert!(
        result.is_err(),
        "Expected ParseError for mismatched tool names"
    );

    if let Err(ref error) = result {
        let error_msg = error.to_string();
        assert!(
            error_msg.contains("mismatching tool names"),
            "Error should mention mismatching tool names: {}",
            error_msg
        );
        assert!(
            error_msg.contains("read_files"),
            "Error should mention read_files: {}",
            error_msg
        );
        assert!(
            error_msg.contains("list_files"),
            "Error should mention list_files: {}",
            error_msg
        );
    }

    Ok(())
}

#[test]
fn test_ignore_non_tool_tags() -> Result<()> {
    let text = concat!(
        "I will work with some HTML code:\n",
        "\n",
        "<div>Some HTML content</div>\n",
        "<tool:read_files>\n",
        "<param:project>test</param:project>\n",
        "<param:path>index.html</param:path>\n",
        "</tool:read_files>\n",
        "<p>More HTML after the tool</p>"
    )
    .to_string();
    let response = LLMResponse {
        content: vec![ContentBlock::Text { text }],
        usage: Usage::zero(),
    };

    let result = parse_llm_response(&response, 1)?;

    // Should successfully parse the tool while ignoring HTML tags
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].name, "read_files");
    assert_eq!(
        result[0].input.get("project").unwrap().as_str().unwrap(),
        "test"
    );
    assert_eq!(
        result[0].input.get("paths").unwrap().as_array().unwrap()[0],
        "index.html"
    );

    Ok(())
}

#[test]
fn test_html_between_tool_tags_should_error() -> Result<()> {
    let text = concat!(
        "I will read files with some HTML mixed in:\n",
        "\n",
        "<tool:read_files>\n",
        "<div>This HTML should not be here</div>\n",
        "<param:project>test</param:project>\n",
        "<param:path>index.html</param:path>\n",
        "</tool:read_files>"
    )
    .to_string();
    let response = LLMResponse {
        content: vec![ContentBlock::Text { text }],
        usage: Usage::zero(),
    };

    let result = parse_llm_response(&response, 1);

    // This should be an error since HTML tags between tool tags (but outside parameters) make the structure unclear
    assert!(
        result.is_err(),
        "Expected ParseError for HTML tag inside tool block"
    );

    if let Err(ref error) = result {
        let error_msg = error.to_string();
        assert!(
            error_msg.contains("unexpected tag"),
            "Error should mention unexpected tag: {}",
            error_msg
        );
        assert!(
            error_msg.contains("div"),
            "Error should mention the div tag: {}",
            error_msg
        );
        assert!(
            error_msg.contains("read_files"),
            "Error should mention the tool name: {}",
            error_msg
        );
    }

    Ok(())
}

#[test]
fn test_html_inside_parameter_allowed() -> Result<()> {
    // The existing test_flexible_xml_parsing already covers HTML content inside parameters
    // We'll just verify that our validation doesn't break that case
    let text = concat!(
        "I will search for content with special characters:\n",
        "\n",
        "<tool:search_files>\n",
        "<param:project>test</param:project>\n",
        "<param:regex><div id=\"test\"></param:regex>\n",
        "</tool:search_files>"
    )
    .to_string();
    let response = LLMResponse {
        content: vec![ContentBlock::Text { text }],
        usage: Usage::zero(),
    };

    let result = parse_llm_response(&response, 1)?;

    // Should successfully parse - special characters inside parameter content are allowed
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].name, "search_files");
    assert_eq!(
        result[0].input.get("project").unwrap().as_str().unwrap(),
        "test"
    );
    assert_eq!(
        result[0].input.get("regex").unwrap().as_str().unwrap(),
        "<div id=\"test\">"
    );

    Ok(())
}

#[tokio::test]
async fn test_unknown_tool_error_handling() -> Result<()> {
    let mock_llm = MockLLMProvider::new(vec![
        Ok(create_test_response(
            "read-files-id",
            "read_files",
            serde_json::json!({
                "project": "test",
                "paths": ["test.txt:1-2"]
            }),
            "Reading file after getting unknown tool error",
        )),
        // Simulate LLM attempting to use unknown tool
        Ok(create_test_response(
            "test-id",
            "unknown_tool",
            serde_json::json!({
                "some_param": "value"
            }),
            "Calling unknown tool",
        )),
    ]);
    let mock_llm_ref = mock_llm.clone();

    let mut agent = Agent::new(
        Box::new(mock_llm),
        ToolMode::Native,
        Box::new(MockProjectManager::new()),
        Box::new(create_command_executor_mock()),
        Arc::new(Box::new(MockUI::default()) as Box<dyn UserInterface>),
        Box::new(MockStatePersistence::new()),
        Some(PathBuf::from("./test_path")),
    );

    agent.start_with_task("Test task".to_string()).await?;

    let requests = mock_llm_ref.get_requests();

    // Should see three requests:
    // 1. Failed unknown tool
    // 2. Corrected ReadFiles
    // 3. CompleteTask
    assert_eq!(requests.len(), 3);

    // Check error was communicated to LLM
    let error_request = &requests[1];
    // Check that we have the expected number of messages in the error request
    assert_eq!(error_request.messages.len(), 3);

    // Check first message (task)
    assert_eq!(error_request.messages[0].role, MessageRole::User);
    if let MessageContent::Text(content) = &error_request.messages[0].content {
        assert_eq!(content, "Test task");
    } else {
        panic!("Expected Text content in first message");
    }

    // Check second message (assistant message with unknown tool)
    assert_eq!(error_request.messages[1].role, MessageRole::Assistant);
    if let MessageContent::Structured(blocks) = &error_request.messages[1].content {
        assert_eq!(blocks.len(), 2);

        // Check first block - text reasoning
        if let ContentBlock::Text { text } = &blocks[0] {
            assert!(text.contains("Calling unknown tool"));
        } else {
            panic!("Expected Text block as first block in assistant message");
        }

        // Check second block - tool use
        if let ContentBlock::ToolUse { id, name, input } = &blocks[1] {
            assert_eq!(id, "test-id");
            assert_eq!(name, "unknown_tool");
            assert_eq!(input["some_param"], "value");
        } else {
            panic!("Expected ToolUse block as second block in assistant message");
        }
    } else {
        panic!("Expected Structured content in second message");
    }

    // Check third message (error response)
    assert_eq!(error_request.messages[2].role, MessageRole::User);
    if let MessageContent::Structured(blocks) = &error_request.messages[2].content {
        assert_eq!(blocks.len(), 1);

        // Check error block
        if let ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } = &blocks[0]
        {
            assert_eq!(tool_use_id, "test-id");
            assert!(is_error.unwrap_or(false));
            assert!(content.contains("unknown_tool"));
            assert!(content.contains("available tools"));
        } else {
            panic!("Expected ToolResult block in error message");
        }
    } else {
        panic!("Expected Structured content in third message");
    }

    Ok(())
}

#[tokio::test]
async fn test_invalid_xml_tool_error_handling() -> Result<()> {
    let mock_llm = MockLLMProvider::new(vec![
        Ok(create_test_response_text("Task completed successfully.")), // Final response after successful tool
        Ok(create_test_response_text(concat!(
            "Correct second attempt:\n",
            "\n",
            "<tool:read_files>\n",
            "<param:project>test</param:project>\n",
            "<param:path>test.txt</param:path>\n",
            "</tool:read_files>"
        ))),
        // Simulate LLM using an invalid tool call with mixed start/end tags
        Ok(create_test_response_text(concat!(
            "Attempting to read a file with invalid tool call:\n",
            "\n",
            "<tool:read_files>\n",
            "<param:project>test</param:project>\n",
            "<param:path>test.txt</param:path>\n",
            "</tool:read>"
        ))),
    ]);
    let mock_llm_ref = mock_llm.clone();

    let mut agent = Agent::new(
        Box::new(mock_llm),
        ToolMode::Xml,
        Box::new(MockProjectManager::new()),
        Box::new(create_command_executor_mock()),
        Arc::new(Box::new(MockUI::default()) as Box<dyn UserInterface>),
        Box::new(MockStatePersistence::new()),
        Some(PathBuf::from("./test_path")),
    );

    // Add an initial user message like the working test does
    let user_msg = Message {
        role: MessageRole::User,
        content: MessageContent::Text("Test task".to_string()),
        request_id: None,
    };
    agent.append_message(user_msg)?;

    agent.run_single_iteration().await?;

    let requests = mock_llm_ref.get_requests();

    // Should see three requests:
    // 1. Initial request with invalid tool
    // 2. Request with corrected tool after error feedback
    // 3. Final request after successful tool execution
    assert_eq!(requests.len(), 3);

    // Verify that we get requests with increasing message counts as expected
    assert_eq!(requests[0].messages.len(), 1); // Initial user message
    assert_eq!(requests[1].messages.len(), 3); // User + Assistant(invalid) + User(error)
    assert_eq!(requests[2].messages.len(), 5); // Previous + Assistant(valid) + User(tool result)

    // Validate Request 1: Invalid XML parse error handling
    let request1 = &requests[1];

    // Check assistant message with invalid XML
    assert_eq!(request1.messages[1].role, MessageRole::Assistant);
    if let MessageContent::Structured(blocks) = &request1.messages[1].content {
        assert_eq!(blocks.len(), 1);
        if let ContentBlock::Text { text } = &blocks[0] {
            assert!(text.contains("invalid tool call"));
            assert!(text.contains("</tool:read>")); // The invalid closing tag
        } else {
            panic!("Expected Text block in assistant message");
        }
    } else {
        panic!("Expected Structured content in assistant message");
    }

    // Check error message from system - in XML mode gets converted to text but should contain our error content
    assert_eq!(request1.messages[2].role, MessageRole::User);
    if let MessageContent::Text(error_text) = &request1.messages[2].content {
        assert!(error_text.contains("Tool error"));
        assert!(error_text.contains("mismatching tool names"));
        assert!(error_text.contains("Expected '</tool:read_files>'"));
        assert!(error_text.contains("found '</tool:read>'"));
        assert!(error_text.contains("Please try again"));
    } else {
        panic!("Expected Text content in error message for XML mode (after conversion)");
    }

    // Validate Request 2: Corrected tool call and successful execution
    let request2 = &requests[2];

    // Check corrected assistant message
    assert_eq!(request2.messages[3].role, MessageRole::Assistant);
    if let MessageContent::Structured(blocks) = &request2.messages[3].content {
        assert_eq!(blocks.len(), 1);
        if let ContentBlock::Text { text } = &blocks[0] {
            assert!(text.contains("Correct second attempt"));
            assert!(text.contains("</tool:read_files>")); // The correct closing tag
        } else {
            panic!("Expected Text block in corrected assistant message");
        }
    } else {
        panic!("Expected Structured content in corrected assistant message");
    }

    // Check successful tool execution result
    assert_eq!(request2.messages[4].role, MessageRole::User);
    if let MessageContent::Text(result_text) = &request2.messages[4].content {
        assert!(result_text.contains("Successfully loaded"));
        assert!(result_text.contains("FILE: test.txt"));
        assert!(result_text.contains("line 1"));
        assert!(result_text.contains("line 2"));
        assert!(result_text.contains("line 3"));
    } else {
        panic!("Expected Text content in tool result message");
    }

    Ok(())
}

#[tokio::test]
async fn test_parse_error_handling() -> Result<()> {
    let mock_llm = MockLLMProvider::new(vec![
        Ok(create_test_response(
            "read-files-2",
            "read_files",
            serde_json::json!({
                "project": "test",
                "paths": ["test.txt"]
            }),
            "Reading with correct parameters",
        )),
        // Simulate LLM sending invalid params
        Ok(create_test_response(
            "read-files-1",
            "read_files",
            serde_json::json!({
                // Missing required 'paths' parameter
                "wrong_param": "value"
            }),
            "Reading with incorrect parameters",
        )),
    ]);
    let mock_llm_ref = mock_llm.clone();

    let mut agent = Agent::new(
        Box::new(mock_llm),
        ToolMode::Native,
        Box::new(MockProjectManager::new()),
        Box::new(create_command_executor_mock()),
        Arc::new(Box::new(MockUI::default()) as Box<dyn UserInterface>),
        Box::new(MockStatePersistence::new()),
        Some(PathBuf::from("./test_path")),
    );

    agent.start_with_task("Test task".to_string()).await?;

    mock_llm_ref.print_requests();
    let requests = mock_llm_ref.get_requests();

    // Should see three requests:
    // 1. Failed parse
    // 2. Corrected ReadFiles
    // 3. CompleteTask
    assert_eq!(requests.len(), 3);

    // Check error was communicated to LLM
    let error_request = &requests[1];
    assert!(error_request.messages.len() >= 2); // May have changed with the new implementation

    // Check that we have the expected number of messages in the error request
    assert_eq!(error_request.messages.len(), 3);

    // Check first message (task)
    assert_eq!(error_request.messages[0].role, MessageRole::User);
    if let MessageContent::Text(content) = &error_request.messages[0].content {
        assert_eq!(content, "Test task");
    } else {
        panic!("Expected Text content in first message");
    }

    // Check second message (assistant message with incorrect parameters)
    assert_eq!(error_request.messages[1].role, MessageRole::Assistant);
    if let MessageContent::Structured(blocks) = &error_request.messages[1].content {
        assert_eq!(blocks.len(), 2);

        // Check first block - text reasoning
        if let ContentBlock::Text { text } = &blocks[0] {
            assert!(text.contains("Reading with incorrect parameters"));
        } else {
            panic!("Expected Text block as first block in assistant message");
        }

        // Check second block - tool use with wrong parameters
        if let ContentBlock::ToolUse { id, name, input } = &blocks[1] {
            assert_eq!(id, "read-files-1");
            assert_eq!(name, "read_files");
            assert!(
                input.get("paths").is_none(),
                "Should not have 'paths' parameter"
            );
            assert_eq!(input["wrong_param"], "value");
        } else {
            panic!("Expected ToolUse block as second block in assistant message");
        }
    } else {
        panic!("Expected Structured content in second message");
    }

    // Check third message (error response)
    assert_eq!(error_request.messages[2].role, MessageRole::User);
    if let MessageContent::Structured(blocks) = &error_request.messages[2].content {
        assert_eq!(blocks.len(), 1);

        // Check error block
        if let ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } = &blocks[0]
        {
            assert_eq!(tool_use_id, "read-files-1");
            assert!(is_error.unwrap_or(false));

            // Check for error content about missing parameters
            let error_content = content.to_lowercase();
            assert!(
                error_content.contains("parameter"),
                "Error should mention parameters: {}",
                content
            );
        } else {
            panic!("Expected ToolResult block in error message");
        }
    } else {
        panic!("Expected Structured content in third message");
    }

    Ok(())
}

#[test]
fn test_failed_tool_id_generation() -> Result<()> {
    // Test that our failed tool ID generation follows the expected format
    use crate::agent::runner::Agent;

    let tool_id = Agent::generate_failed_tool_id(123, 0);
    assert_eq!(tool_id, "failed-tool-123-0");

    let tool_id2 = Agent::generate_failed_tool_id(456, 2);
    assert_eq!(tool_id2, "failed-tool-456-2");

    Ok(())
}

#[test]
fn test_ui_filtering_with_failed_tool_messages() -> Result<()> {
    use crate::persistence::ChatSession;
    use crate::session::instance::SessionInstance;
    use std::time::SystemTime;

    // Create a session with mixed messages including failed tool error messages
    let session = ChatSession {
        id: "test-session".to_string(),
        name: "Test Session".to_string(),
        created_at: SystemTime::now(),
        updated_at: SystemTime::now(),
        messages: vec![
            // Regular user message - should be included
            Message {
                role: MessageRole::User,
                content: MessageContent::Text("Hello, please help me".to_string()),
                request_id: None,
            },
            // Assistant response
            Message {
                role: MessageRole::Assistant,
                content: MessageContent::Text("I'll help you".to_string()),
                request_id: Some(1),
            },
            // Failed tool error message in XML mode - should be filtered out
            Message {
                role: MessageRole::User,
                content: MessageContent::Structured(vec![ContentBlock::ToolResult {
                    tool_use_id: "failed-tool-1-0".to_string(),
                    content:
                        "Tool error: Unknown tool 'invalid_tool'. Please use only available tools."
                            .to_string(),
                    is_error: Some(true),
                }]),
                request_id: None,
            },
            // Regular tool result - should be filtered out
            Message {
                role: MessageRole::User,
                content: MessageContent::Structured(vec![ContentBlock::ToolResult {
                    tool_use_id: "regular-tool-123".to_string(),
                    content: "File contents here".to_string(),
                    is_error: None,
                }]),
                request_id: None,
            },
            // Empty user message (legacy) - should be filtered out
            Message {
                role: MessageRole::User,
                content: MessageContent::Text("".to_string()),
                request_id: None,
            },
            // Another regular user message - should be included
            Message {
                role: MessageRole::User,
                content: MessageContent::Text("Thank you for the help!".to_string()),
                request_id: None,
            },
        ],
        tool_executions: Vec::new(),
        working_memory: crate::types::WorkingMemory::default(),
        init_path: None,
        initial_project: None,
        tool_mode: ToolMode::Xml,
        next_request_id: 1,
    };

    let session_instance = SessionInstance::new(session);

    // Test the UI message conversion - should filter out tool-result and empty messages
    let ui_messages = session_instance.convert_messages_to_ui_data(ToolMode::Xml)?;

    // Should only have 3 messages:
    // 1. "Hello, please help me" (user)
    // 2. "I'll help you" (assistant)
    // 3. "Thank you for the help!" (user)
    assert_eq!(ui_messages.len(), 3);

    // Verify the first message
    assert_eq!(
        ui_messages[0].role,
        crate::ui::gpui::elements::MessageRole::User
    );
    assert!(ui_messages[0].fragments.iter().any(|f| match f {
        crate::ui::streaming::DisplayFragment::PlainText(text) =>
            text.contains("Hello, please help me"),
        _ => false,
    }));

    // Verify the second message
    assert_eq!(
        ui_messages[1].role,
        crate::ui::gpui::elements::MessageRole::Assistant
    );
    assert!(ui_messages[1].fragments.iter().any(|f| match f {
        crate::ui::streaming::DisplayFragment::PlainText(text) => text.contains("I'll help you"),
        _ => false,
    }));

    // Verify the third message
    assert_eq!(
        ui_messages[2].role,
        crate::ui::gpui::elements::MessageRole::User
    );
    assert!(ui_messages[2].fragments.iter().any(|f| match f {
        crate::ui::streaming::DisplayFragment::PlainText(text) =>
            text.contains("Thank you for the help!"),
        _ => false,
    }));

    Ok(())
}
