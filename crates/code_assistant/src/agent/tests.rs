use super::*;
use crate::agent::persistence::MockStatePersistence;
use crate::session::SessionConfig;
use crate::tests::mocks::MockLLMProvider;
use crate::tests::mocks::{
    create_command_executor_mock, create_test_response, create_test_response_text,
    MockProjectManager, MockUI,
};
use crate::tests::utils::parse_and_truncate_llm_response;
use crate::types::*;
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
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    // Use a test request_id
    let request_id = 42;

    let (tool_requests, _truncated_response) =
        parse_and_truncate_llm_response(&response, request_id)?;
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
        "<tool:edit>\n",
        "<param:project>test</param:project>\n",
        "<param:path>src/main.rs</param:path>\n",
        "<param:old_text>function test(){\n",
        "  console.log(\"messy\");\n",
        "}</param:old_text>\n",
        "<param:new_text>function test() {\n",
        "    console.log(\"clean\");\n",
        "}</param:new_text>\n",
        "</tool:edit>\n",
    )
    .to_string();
    let response = LLMResponse {
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    // Use a test request_id
    let request_id = 42;
    let (tool_requests, _truncated_response) =
        parse_and_truncate_llm_response(&response, request_id)?;
    assert_eq!(tool_requests.len(), 1);

    let request = &tool_requests[0];
    assert_eq!(request.name, "edit");
    assert_eq!(
        request.input.get("project").unwrap().as_str().unwrap(),
        "test"
    );
    assert_eq!(
        request.input.get("path").unwrap().as_str().unwrap(),
        "src/main.rs"
    );

    let old_text = request.input.get("old_text").unwrap().as_str().unwrap();
    assert!(old_text.contains("function test(){"));
    assert!(old_text.contains("console.log(\"messy\")"));

    let new_text = request.input.get("new_text").unwrap().as_str().unwrap();
    assert!(new_text.contains("function test() {"));
    assert!(new_text.contains("console.log(\"clean\")"));

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
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let result = parse_and_truncate_llm_response(&response, 1);
    println!("result: {result:?}");

    // This should return an error, not Ok([])
    assert!(
        result.is_err(),
        "Expected ParseError for mismatched tool names"
    );

    if let Err(ref error) = result {
        let error_msg = error.to_string();
        assert!(
            error_msg.contains("mismatching tool names"),
            "Error should mention mismatching tool names: {error_msg}"
        );
        assert!(
            error_msg.contains("read_files"),
            "Error should mention read_files: {error_msg}"
        );
        assert!(
            error_msg.contains("list_files"),
            "Error should mention list_files: {error_msg}"
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_missing_closing_param_tag() -> Result<()> {
    let text = concat!(
        "Let me examine the current parsing logic more closely and then fix it:\n",
        "\n",
        "<tool:replace_in_file>\n",
        "<param:project>code-assistant</param:project>\n",
        "<param:path>crates/llm/src/openai.rs</param:path>\n",
        "<param:diff>\n",
        "<<<<<<< SEARCH\n",
        "        fn parse_duration(headers: &reqwest::header::HeaderMap, name: &str) -> Option<Duration> {\n",
        "            headers.get(name).and_then(|h| h.to_str().ok()).map(|s| {\n",
        "                // Parse OpenAI's duration format (e.g., \"1s\", \"6m0s\")\n",
        "                let mut seconds = 0u64;\n",
        "                let mut current_num = String::new();\n",
        "\n",
        "                for c in s.chars() {\n",
        "                    match c {\n",
        "                        '0'..='9' => current_num.push(c),\n",
        "                        'm' => {\n",
        "                            if let Ok(mins) = current_num.parse::<u64>() {\n",
        "                                seconds += mins * 60;\n",
        "                            }\n",
        "                            current_num.clear();\n",
        "                        }\n",
        "                        's' => {\n",
        "                            if let Ok(secs) = current_num.parse::<u64>() {\n",
        "                                seconds += secs;\n",
        "                            }\n",
        "                            current_num.clear();\n",
        "                        }\n",
        "                        _ => current_num.clear(),\n",
        "                    }\n",
        "                }\n",
        "                Duration::from_secs(seconds)\n",
        "            })\n",
        "        }\n",
        "=======\n",
        "        fn parse_duration(headers: &reqwest::header::HeaderMap, name: &str) -> Option<Duration> {\n",
        "            headers.get(name).and_then(|h| h.to_str().ok()).map(|s| {\n",
        "                // Parse OpenAI's duration format (e.g., \"1s\", \"6m0s\", \"7.66s\", \"2m59.56s\")\n",
        "                let mut total_seconds = 0.0f64;\n",
        "                let mut current_num = String::new();\n",
        "                \n",
        "                for c in s.chars() {\n",
        "                    match c {\n",
        "                        '0'..='9' | '.' => current_num.push(c),\n",
        "                        'm' => {\n",
        "                            if let Ok(mins) = current_num.parse::<f64>() {\n",
        "                                total_seconds += mins * 60.0;\n",
        "                            }\n",
        "                            current_num.clear();\n",
        "                        }\n",
        "                        's' => {\n",
        "                            if let Ok(secs) = current_num.parse::<f64>() {\n",
        "                                total_seconds += secs;\n",
        "                            }\n",
        "                            current_num.clear();\n",
        "                        }\n",
        "                        _ => current_num.clear(),\n",
        "                    }\n",
        "                }\n",
        "                Duration::from_secs_f64(total_seconds)\n",
        "            })\n",
        "        }\n",
        ">>>>>>> REPLACE\n",
        "</tool:replace_in_file>\n",
    )
    .to_string();
    let response = LLMResponse {
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let result = parse_and_truncate_llm_response(&response, 1);
    println!("result: {result:?}");

    // This should return an error, not Ok([])
    assert!(
        result.is_err(),
        "Expected ParseError for missing </param:diff> close tag"
    );

    // if let Err(ref error) = result {
    //     let error_msg = error.to_string();
    //     assert!(
    //         error_msg.contains("</param:diff>"),
    //         "Error should mention missing closing tag: {}",
    //         error_msg
    //     );
    // }

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
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let (result, _truncated_response) = parse_and_truncate_llm_response(&response, 1)?;

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
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let result = parse_and_truncate_llm_response(&response, 1);

    // This should be an error since HTML tags between tool tags (but outside parameters) make the structure unclear
    assert!(
        result.is_err(),
        "Expected ParseError for HTML tag inside tool block"
    );

    if let Err(ref error) = result {
        let error_msg = error.to_string();
        assert!(
            error_msg.contains("unexpected tag"),
            "Error should mention unexpected tag: {error_msg}"
        );
        assert!(
            error_msg.contains("div"),
            "Error should mention the div tag: {error_msg}"
        );
        assert!(
            error_msg.contains("read_files"),
            "Error should mention the tool name: {error_msg}"
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
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let (result, _truncated_response) = parse_and_truncate_llm_response(&response, 1)?;

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

    let components = AgentComponents {
        llm_provider: Box::new(mock_llm),
        project_manager: Box::new(MockProjectManager::new()),
        command_executor: Box::new(create_command_executor_mock()),
        ui: Arc::new(MockUI::default()),
        state_persistence: Box::new(MockStatePersistence::new()),
    };

    let session_config = SessionConfig {
        init_path: Some(PathBuf::from("./test_path")),
        initial_project: String::new(),
        tool_syntax: ToolSyntax::Native,
        use_diff_blocks: false,
    };

    let mut agent = Agent::new(components, session_config);
    agent.disable_naming_reminders();

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
        if let ContentBlock::Text { text, .. } = &blocks[0] {
            assert!(text.contains("Calling unknown tool"));
        } else {
            panic!("Expected Text block as first block in assistant message");
        }

        // Check second block - tool use
        if let ContentBlock::ToolUse {
            id, name, input, ..
        } = &blocks[1]
        {
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
            ..
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

    let components = AgentComponents {
        llm_provider: Box::new(mock_llm),
        project_manager: Box::new(MockProjectManager::new()),
        command_executor: Box::new(create_command_executor_mock()),
        ui: Arc::new(MockUI::default()),
        state_persistence: Box::new(MockStatePersistence::new()),
    };

    let session_config = SessionConfig {
        init_path: Some(PathBuf::from("./test_path")),
        initial_project: String::new(),
        tool_syntax: ToolSyntax::Xml,
        use_diff_blocks: false,
    };

    let mut agent = Agent::new(components, session_config);
    agent.disable_naming_reminders();

    // Add an initial user message like the working test does
    let user_msg = Message {
        role: MessageRole::User,
        content: MessageContent::Text("Test task".to_string()),
        request_id: None,
        usage: None,
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
        if let ContentBlock::Text { text, .. } = &blocks[0] {
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
        if let ContentBlock::Text { text, .. } = &blocks[0] {
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

    let components = AgentComponents {
        llm_provider: Box::new(mock_llm),
        project_manager: Box::new(MockProjectManager::new()),
        command_executor: Box::new(create_command_executor_mock()),
        ui: Arc::new(MockUI::default()),
        state_persistence: Box::new(MockStatePersistence::new()),
    };

    let session_config = SessionConfig {
        init_path: Some(PathBuf::from("./test_path")),
        initial_project: String::new(),
        tool_syntax: ToolSyntax::Native,
        use_diff_blocks: false,
    };

    let mut agent = Agent::new(components, session_config);
    agent.disable_naming_reminders();

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
        if let ContentBlock::Text { text, .. } = &blocks[0] {
            assert!(text.contains("Reading with incorrect parameters"));
        } else {
            panic!("Expected Text block as first block in assistant message");
        }

        // Check second block - tool use with wrong parameters
        if let ContentBlock::ToolUse {
            id, name, input, ..
        } = &blocks[1]
        {
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
            ..
        } = &blocks[0]
        {
            assert_eq!(tool_use_id, "read-files-1");
            assert!(is_error.unwrap_or(false));

            // Check for error content about missing parameters
            let error_content = content.to_lowercase();
            assert!(
                error_content.contains("parameter"),
                "Error should mention parameters: {content}"
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
fn test_ui_filtering_with_failed_tool_messages() -> Result<()> {
    use crate::persistence::ChatSession;
    use crate::session::instance::SessionInstance;

    // Create a session with mixed messages including failed tool error messages
    let mut session = ChatSession::new_empty(
        "test-session".to_string(),
        "Test Session".to_string(),
        SessionConfig {
            init_path: None,
            initial_project: String::new(),
            tool_syntax: ToolSyntax::Xml,
            use_diff_blocks: false,
        },
        None,
    );
    session.messages = vec![
        // Regular user message - should be included
        Message {
            role: MessageRole::User,
            content: MessageContent::Text("Hello, please help me".to_string()),
            request_id: None,
            usage: None,
        },
        // Assistant response
        Message {
            role: MessageRole::Assistant,
            content: MessageContent::Text("I'll help you".to_string()),
            request_id: Some(1),
            usage: None,
        },
        // Parse error message in XML mode - should be filtered out
        Message {
            role: MessageRole::User,
            content: MessageContent::Structured(vec![ContentBlock::new_error_tool_result(
                "tool-1-0",
                "Tool error: Unknown tool 'invalid_tool'. Please use only available tools.",
            )]),
            request_id: None,
            usage: None,
        },
        // Regular tool result - should be filtered out
        Message {
            role: MessageRole::User,
            content: MessageContent::Structured(vec![ContentBlock::new_tool_result(
                "regular-tool-123",
                "File contents here",
            )]),
            request_id: None,
            usage: None,
        },
        // Empty user message (legacy) - should be filtered out
        Message {
            role: MessageRole::User,
            content: MessageContent::Text("".to_string()),
            request_id: None,
            usage: None,
        },
        // Another regular user message - should be included
        Message {
            role: MessageRole::User,
            content: MessageContent::Text("Thank you for the help!".to_string()),
            request_id: None,
            usage: None,
        },
    ];
    session.tool_executions = Vec::new();
    session.working_memory = crate::types::WorkingMemory::default();
    session.next_request_id = 1;

    let session_instance = SessionInstance::new(session);

    // Test the UI message conversion - should filter out tool-result and empty messages
    let ui_messages = session_instance.convert_messages_to_ui_data(ToolSyntax::Xml)?;

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

#[test]
fn test_caret_array_parsing() -> Result<()> {
    use crate::tools::ParserRegistry;

    let text = concat!(
        "^^^read_files\n",
        "project: code-assistant\n",
        "paths: [\n",
        "docs/customizable-tool-syntax.md\n",
        "]\n",
        "^^^"
    );

    let response = LLMResponse {
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let parser = ParserRegistry::get(ToolSyntax::Caret);
    let (tool_requests, _truncated_response) = parser.extract_requests(&response, 123, 0)?;

    assert_eq!(tool_requests.len(), 1);
    assert_eq!(tool_requests[0].name, "read_files");
    assert_eq!(
        tool_requests[0]
            .input
            .get("project")
            .unwrap()
            .as_str()
            .unwrap(),
        "code-assistant"
    );

    // This should be an array, not a string
    let paths = tool_requests[0].input.get("paths").unwrap();
    println!("paths value: {paths:?}");
    println!("paths type: {paths:?}");

    if paths.is_array() {
        let paths_array = paths.as_array().unwrap();
        assert_eq!(paths_array.len(), 1);
        assert_eq!(paths_array[0], "docs/customizable-tool-syntax.md");
    } else {
        panic!("Expected paths to be an array, but got: {paths:?}");
    }

    Ok(())
}

#[test]
fn test_caret_empty_array_parsing() -> Result<()> {
    use crate::tools::ParserRegistry;

    let text = concat!(
        "^^^read_files\n",
        "project: code-assistant\n",
        "paths: [\n",
        "]\n",
        "^^^"
    );

    let response = LLMResponse {
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let parser = ParserRegistry::get(ToolSyntax::Caret);
    let (tool_requests, _truncated_response) = parser.extract_requests(&response, 123, 0)?;

    assert_eq!(tool_requests.len(), 1);
    assert_eq!(tool_requests[0].name, "read_files");

    // Empty array should still be an array
    let paths = tool_requests[0].input.get("paths").unwrap();
    assert!(paths.is_array());
    assert_eq!(paths.as_array().unwrap().len(), 0);

    Ok(())
}

#[test]
fn test_caret_multiple_arrays_parsing() -> Result<()> {
    use crate::tools::ParserRegistry;

    let text = concat!(
        "^^^search_files\n",
        "project: code-assistant\n",
        "paths: [\n",
        "src/\n",
        "docs/\n",
        "]\n",
        "regex: single-value\n",
        "extensions: [\n",
        "rs\n",
        "md\n",
        "toml\n",
        "]\n",
        "^^^"
    );

    let response = LLMResponse {
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let parser = ParserRegistry::get(ToolSyntax::Caret);
    let (tool_requests, _truncated_response) = parser.extract_requests(&response, 123, 0)?;

    assert_eq!(tool_requests.len(), 1);
    assert_eq!(tool_requests[0].name, "search_files");

    // Check single value parameter
    let regex = tool_requests[0].input.get("regex").unwrap();
    assert!(regex.is_string());
    assert_eq!(regex.as_str().unwrap(), "single-value");

    // Check first array parameter
    let paths = tool_requests[0].input.get("paths").unwrap();
    assert!(paths.is_array());
    let paths_array = paths.as_array().unwrap();
    assert_eq!(paths_array.len(), 2);
    assert_eq!(paths_array[0], "src/");
    assert_eq!(paths_array[1], "docs/");

    // Check second array parameter
    let extensions = tool_requests[0].input.get("extensions").unwrap();
    assert!(extensions.is_array());
    let ext_array = extensions.as_array().unwrap();
    assert_eq!(ext_array.len(), 3);
    assert_eq!(ext_array[0], "rs");
    assert_eq!(ext_array[1], "md");
    assert_eq!(ext_array[2], "toml");

    Ok(())
}

#[test]
fn test_caret_array_with_multiline_parsing() -> Result<()> {
    use crate::tools::ParserRegistry;

    let text = concat!(
        "^^^write_file\n",
        "project: code-assistant\n",
        "path: test.txt\n",
        "tags: [\n",
        "important\n",
        "test-file\n",
        "]\n",
        "content ---\n",
        "This is the file content\n",
        "with multiple lines\n",
        "--- content\n",
        "^^^"
    );

    let response = LLMResponse {
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let parser = ParserRegistry::get(ToolSyntax::Caret);
    let (tool_requests, _truncated_response) = parser.extract_requests(&response, 123, 0)?;

    assert_eq!(tool_requests.len(), 1);
    assert_eq!(tool_requests[0].name, "write_file");

    // Check single parameters
    assert_eq!(
        tool_requests[0]
            .input
            .get("project")
            .unwrap()
            .as_str()
            .unwrap(),
        "code-assistant"
    );
    assert_eq!(
        tool_requests[0]
            .input
            .get("path")
            .unwrap()
            .as_str()
            .unwrap(),
        "test.txt"
    );

    // Check array parameter
    let tags = tool_requests[0].input.get("tags").unwrap();
    assert!(tags.is_array());
    let tags_array = tags.as_array().unwrap();
    assert_eq!(tags_array.len(), 2);
    assert_eq!(tags_array[0], "important");
    assert_eq!(tags_array[1], "test-file");

    // Check multiline parameter
    let content = tool_requests[0].input.get("content").unwrap();
    assert!(content.is_string());
    assert_eq!(
        content.as_str().unwrap(),
        "This is the file content\nwith multiple lines"
    );

    Ok(())
}

#[test]
fn test_original_caret_issue_reproduction() -> Result<()> {
    use crate::tools::ParserRegistry;

    // This is the exact block that was reported as failing
    let text = concat!(
        "^^^read_files\n",
        "project: code-assistant\n",
        "paths: [\n",
        "docs/customizable-tool-syntax.md\n",
        "]\n",
        "^^^"
    );

    let response = LLMResponse {
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let parser = ParserRegistry::get(ToolSyntax::Caret);
    let result = parser.extract_requests(&response, 123, 0);

    match result {
        Ok((tool_requests, _truncated_response)) => {
            assert_eq!(tool_requests.len(), 1);
            assert_eq!(tool_requests[0].name, "read_files");
            assert_eq!(
                tool_requests[0]
                    .input
                    .get("project")
                    .unwrap()
                    .as_str()
                    .unwrap(),
                "code-assistant"
            );

            // This was the original issue - paths should be parsed as an array, not a string
            let paths = tool_requests[0].input.get("paths").unwrap();

            // Before the fix, this would fail with: "invalid type: string, expected a sequence"
            // Now it should work correctly
            assert!(paths.is_array(), "paths should be an array, not a string");
            let paths_array = paths.as_array().unwrap();
            assert_eq!(paths_array.len(), 1);
            assert_eq!(paths_array[0], "docs/customizable-tool-syntax.md");

            println!("✅ Original issue has been fixed!");
            println!("   paths parsed as: {paths:?}");
        }
        Err(e) => {
            panic!("Parser should not fail anymore, but got error: {e}");
        }
    }

    Ok(())
}

#[test]
fn test_inject_naming_reminder_skips_tool_result_messages() -> Result<()> {
    // This test verifies that:
    // 1. Naming reminders are only added to actual user messages, not tool result messages
    // 2. Text messages are converted to Structured with separate ContentBlocks for original text and reminder
    // 3. Structured messages get the reminder added as an additional ContentBlock
    // Create a mock agent for testing
    let llm_provider = Box::new(MockLLMProvider::new(vec![]));
    let project_manager = Box::new(MockProjectManager::default());
    let command_executor = Box::new(create_command_executor_mock());
    let ui = Arc::new(MockUI::default());
    let state_persistence = Box::new(MockStatePersistence::new());

    let components = AgentComponents {
        llm_provider,
        project_manager,
        command_executor,
        ui,
        state_persistence,
    };

    let session_config = SessionConfig {
        init_path: None,
        initial_project: String::new(),
        tool_syntax: ToolSyntax::Xml,
        use_diff_blocks: false,
    };

    let mut agent = Agent::new(components, session_config);

    // Test case 1: User message with text content should get reminder
    let messages = vec![Message {
        role: MessageRole::User,
        content: MessageContent::Text("Hello, help me with a task".to_string()),
        request_id: None,
        usage: None,
    }];

    let result_messages = agent.inject_naming_reminder_if_needed(messages.clone());
    assert_eq!(result_messages.len(), 1);

    // The message should now be structured with two ContentBlocks
    if let MessageContent::Structured(blocks) = &result_messages[0].content {
        assert_eq!(blocks.len(), 2);

        // First block should contain the original user text
        if let ContentBlock::Text { text, .. } = &blocks[0] {
            assert_eq!(text, "Hello, help me with a task");
        } else {
            panic!("Expected first block to be text with original user message");
        }

        // Second block should contain the reminder
        if let ContentBlock::Text { text, .. } = &blocks[1] {
            assert!(text.contains("<system-reminder>"));
            assert!(text.contains("name_session"));
        } else {
            panic!("Expected second block to be text with reminder");
        }
    } else {
        panic!("Expected structured content after reminder injection");
    }

    // Test case 2: User message with only tool results should be skipped
    let messages_with_tool_results = vec![
        Message {
            role: MessageRole::User,
            content: MessageContent::Text("Hello, help me with a task".to_string()),
            request_id: None,
            usage: None,
        },
        Message {
            role: MessageRole::Assistant,
            content: MessageContent::Structured(vec![ContentBlock::new_text(
                "I'll help you with that task.",
            )]),
            request_id: Some(1),
            usage: Some(Usage::zero()),
        },
        Message {
            role: MessageRole::User,
            content: MessageContent::Structured(vec![ContentBlock::new_tool_result(
                "tool-1-1",
                "Tool execution result",
            )]),
            request_id: None,
            usage: None,
        },
    ];

    let result_messages =
        agent.inject_naming_reminder_if_needed(messages_with_tool_results.clone());
    assert_eq!(result_messages.len(), 3);

    // The reminder should be added to the first user message (with text content), not the tool result message
    // The first message should now be structured with two ContentBlocks
    if let MessageContent::Structured(blocks) = &result_messages[0].content {
        assert_eq!(blocks.len(), 2);

        // First block should contain the original user text
        if let ContentBlock::Text { text, .. } = &blocks[0] {
            assert_eq!(text, "Hello, help me with a task");
        } else {
            panic!("Expected first block to be text with original user message");
        }

        // Second block should contain the reminder
        if let ContentBlock::Text { text, .. } = &blocks[1] {
            assert!(text.contains("<system-reminder>"));
            assert!(text.contains("name_session"));
        } else {
            panic!("Expected second block to be text with reminder");
        }
    } else {
        panic!("Expected structured content in first message after reminder injection");
    }

    // The tool result message should remain unchanged
    if let MessageContent::Structured(blocks) = &result_messages[2].content {
        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0], ContentBlock::ToolResult { .. }));
        // No reminder should be added to this message
        for block in blocks {
            if let ContentBlock::Text { text, .. } = block {
                assert!(!text.contains("<system-reminder>"));
            }
        }
    } else {
        panic!("Expected structured content in tool result message");
    }

    // Test case 3: User message with mixed content (text + tool results) should get reminder
    let mixed_message = vec![Message {
        role: MessageRole::User,
        content: MessageContent::Structured(vec![
            ContentBlock::new_text("Please analyze this file"),
            ContentBlock::new_tool_result("tool-1-1", "Previous tool result"),
        ]),
        request_id: None,
        usage: None,
    }];

    let result_messages = agent.inject_naming_reminder_if_needed(mixed_message.clone());
    assert_eq!(result_messages.len(), 1);

    if let MessageContent::Structured(blocks) = &result_messages[0].content {
        assert_eq!(blocks.len(), 3); // Original text + tool result + reminder text

        // First block should be the original text
        if let ContentBlock::Text { text, .. } = &blocks[0] {
            assert_eq!(text, "Please analyze this file");
        } else {
            panic!("Expected first block to be original text");
        }

        // Second block should be the tool result (unchanged)
        assert!(matches!(blocks[1], ContentBlock::ToolResult { .. }));

        // Third block should be the reminder
        if let ContentBlock::Text { text, .. } = &blocks[2] {
            assert!(text.contains("<system-reminder>"));
            assert!(text.contains("name_session"));
        } else {
            panic!("Expected third block to be reminder text");
        }
    } else {
        panic!("Expected structured content");
    }

    // Test case 4: No reminder should be added if session is already named
    agent.set_session_name("Test Session".to_string());
    let result_messages = agent.inject_naming_reminder_if_needed(messages);
    assert_eq!(result_messages.len(), 1);

    // When session is already named, the message should remain unchanged (Text content)
    if let MessageContent::Text(text) = &result_messages[0].content {
        assert_eq!(text, "Hello, help me with a task");
        assert!(!text.contains("<system-reminder>"));
    } else {
        panic!("Expected text content to remain unchanged when session is already named");
    }

    Ok(())
}

#[test]
fn test_update_tool_call_in_text_with_offsets() -> Result<()> {
    use crate::agent::runner::Agent;
    use crate::agent::ToolSyntax;
    use crate::tools::ToolRequest;
    use serde_json::json;

    // Test XML syntax with offset replacement
    let original_text = concat!(
        "I'll write the file for you.\n",
        "\n",
        "<tool:write_file>\n",
        "<param:project>test-project</param:project>\n",
        "<param:path>some_file.ts</param:path>\n",
        "<param:content>\n",
        "console.log('result:',1+1);\n",
        "</param:content>\n",
        "</tool:write_file>\n",
        "\n",
        "Let me know if you need anything else."
    );

    // Parse the original text using the XML parser to extract the actual tool block with offsets
    use crate::tools::parser_registry::ParserRegistry;
    use llm::{ContentBlock, LLMResponse, Usage};

    let parser = ParserRegistry::get(ToolSyntax::Xml);
    let llm_response = LLMResponse {
        content: vec![ContentBlock::new_text(original_text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let (parsed_tools, _) = parser.extract_requests(&llm_response, 123, 0)?;
    assert_eq!(parsed_tools.len(), 1);

    let parsed_tool = &parsed_tools[0];
    assert_eq!(parsed_tool.name, "write_file");
    assert!(parsed_tool.start_offset.is_some());
    assert!(parsed_tool.end_offset.is_some());

    // Create an updated request using the parsed tool's ID and offsets, but with new input
    let updated_request = ToolRequest {
        id: parsed_tool.id.clone(),
        name: parsed_tool.name.clone(),
        input: json!({
            "project": "test-project",
            "path": "some_file.ts",
            // Simulate content has been formatted on save
            "content": "console.log(\"result:\", 1 + 1)",
        }),
        start_offset: parsed_tool.start_offset,
        end_offset: parsed_tool.end_offset,
    };

    // Build expected text by simulating what the formatter would produce
    // The XML formatter adds a trailing newline after </tool:name>, which creates an extra newline
    let expected_text = concat!(
        "I'll write the file for you.\n",
        "\n",
        "<tool:write_file>\n",
        "<param:project>test-project</param:project>\n",
        "<param:path>some_file.ts</param:path>\n",
        "<param:content>\n",
        "console.log(\"result:\", 1 + 1)\n",
        "</param:content>\n",
        "</tool:write_file>\n", // Formatter adds this newline
        "\n",                   // Original newline from text
        "\n",                   // This creates an extra newline due to formatter
        "Let me know if you need anything else."
    );

    let result =
        Agent::update_tool_call_in_text_static(original_text, &updated_request, ToolSyntax::Xml)?;

    // Should have replaced the tool block exactly
    assert_eq!(expected_text, result);

    Ok(())
}

#[test]
fn test_update_tool_call_in_text_caret_syntax() -> Result<()> {
    use crate::agent::runner::Agent;
    use crate::agent::ToolSyntax;
    use crate::tools::ToolRequest;
    use serde_json::json;

    // Test Caret syntax with offset replacement
    let original_text = concat!(
        "Let me write a file for you.\n",
        "\n",
        "^^^write_file\n",
        "project: old-project\n",
        "path: old-file.txt\n",
        "content: old content\n",
        "^^^\n",
        "\n",
        "Done!"
    );

    // Parse the original text using the Caret parser to extract the actual tool block with offsets
    use crate::tools::parser_registry::ParserRegistry;
    use llm::{ContentBlock, LLMResponse, Usage};

    let parser = ParserRegistry::get(ToolSyntax::Caret);
    let llm_response = LLMResponse {
        content: vec![ContentBlock::new_text(original_text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    };

    let (parsed_tools, _) = parser.extract_requests(&llm_response, 456, 0)?;
    assert_eq!(parsed_tools.len(), 1);

    let parsed_tool = &parsed_tools[0];
    assert_eq!(parsed_tool.name, "write_file");
    assert!(parsed_tool.start_offset.is_some());
    assert!(parsed_tool.end_offset.is_some());

    // Create an updated request using the parsed tool's ID and offsets, but with new input
    let updated_request = ToolRequest {
        id: parsed_tool.id.clone(),
        name: parsed_tool.name.clone(),
        input: json!({
            "project": "new-project",
            "path": "new-file.txt",
            "content": "new content here"
        }),
        start_offset: parsed_tool.start_offset,
        end_offset: parsed_tool.end_offset,
    };

    // Build expected text by simulating what the formatter would produce
    // The Caret formatter adds a trailing newline after ^^^, which creates an extra newline
    let expected_text = concat!(
        "Let me write a file for you.\n",
        "\n",
        "^^^write_file\n",
        "project: new-project\n",
        "path: new-file.txt\n",
        "content ---\n",
        "new content here\n",
        "--- content\n",
        "^^^\n", // Formatter adds this newline
        "\n",    // Original newline from text
        "\n",    // This creates an extra newline due to formatter
        "Done!"
    );

    let result =
        Agent::update_tool_call_in_text_static(original_text, &updated_request, ToolSyntax::Caret)?;

    // Should have replaced the tool block exactly
    assert_eq!(expected_text, result);

    Ok(())
}

#[test]
fn test_update_tool_call_in_text_fallback_mode() -> Result<()> {
    use crate::agent::runner::Agent;
    use crate::agent::ToolSyntax;
    use crate::tools::ToolRequest;
    use serde_json::json;

    // Test fallback mode when offsets are missing
    let original_text = "Here's some original text with a tool call.";

    let updated_request = ToolRequest {
        id: "tool-789-1".to_string(),
        name: "read_files".to_string(),
        input: json!({
            "project": "test-project",
            "paths": ["test-file.txt"]
        }),
        start_offset: None, // No offset information
        end_offset: None,
    };

    let result =
        Agent::update_tool_call_in_text_static(original_text, &updated_request, ToolSyntax::Xml)?;

    // Should have appended the updated tool call
    assert!(result.contains(original_text));
    assert!(result.contains("<!-- Tool call tool-789-1 was updated after auto-formatting -->"));
    assert!(result.contains("<tool:read_files>"));
    assert!(result.contains("<param:project>test-project</param:project>"));
    assert!(result.contains("<param:path>test-file.txt</param:path>"));
    assert!(result.contains("</tool:read_files>"));

    Ok(())
}
