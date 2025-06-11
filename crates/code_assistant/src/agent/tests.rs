use super::*;
use crate::agent::runner::parse_llm_response;
use crate::persistence::FileStatePersistence;
use crate::session::SessionManager;
use crate::tests::mocks::MockLLMProvider;
use crate::tests::mocks::{
    create_command_executor_mock, create_test_response, MockProjectManager, MockUI,
};
use crate::types::*;
use crate::UserInterface;
use anyhow::Result;
use llm::types::*;
use std::path::PathBuf;
use std::sync::Arc;

/// Create a test SessionManager with a temporary directory
fn create_test_session_manager() -> SessionManager {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!(
        "code_assistant_test_{}_{}",
        std::process::id(),
        timestamp
    ));
    let persistence = FileStatePersistence::new(temp_dir);
    SessionManager::new(persistence)
}

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
        create_test_session_manager(),
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
        create_test_session_manager(),
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
