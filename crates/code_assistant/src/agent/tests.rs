use super::*;
use crate::agent::agent::parse_llm_response;
use crate::persistence::MockStatePersistence;
use crate::tests::mocks::MockLLMProvider;
use crate::tests::mocks::{
    create_command_executor_mock, create_test_response, MockProjectManager, MockUI,
};
use crate::types::*;
use anyhow::Result;
use llm::types::*;
use std::path::PathBuf;

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
            "read_files",
            serde_json::json!({
                "project": "test",
                "paths": ["test.txt"]
            }),
            "Reading file after getting unknown tool error",
        )),
        // Simulate LLM attempting to use unknown tool
        Ok(LLMResponse {
            content: vec![ContentBlock::ToolUse {
                id: "test-id".to_string(),
                name: "unknown_tool".to_string(),
                input: serde_json::json!({}),
            }],
            usage: Usage::zero(),
        }),
    ]);
    let mock_llm_ref = mock_llm.clone();

    let mut agent = Agent::new(
        Box::new(mock_llm),
        ToolMode::Native,
        Box::new(MockProjectManager::new()),
        Box::new(create_command_executor_mock()),
        Box::new(MockUI::default()),
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
    assert!(error_request.messages.len() >= 2); // May have changed with the new implementation

    // Find the error message
    let error_message = error_request.messages.iter().find_map(|msg| {
        if let MessageContent::Text(content) = &msg.content {
            if content.contains("Error") || content.contains("error") {
                Some(content)
            } else {
                None
            }
        } else {
            None
        }
    });

    assert!(
        error_message.is_some(),
        "Error message not found in response"
    );
    assert!(error_message.unwrap().contains("unknown_tool"));
    assert!(error_message.unwrap().contains("available tools"));

    Ok(())
}

#[tokio::test]
async fn test_parse_error_handling() -> Result<()> {
    let mock_llm = MockLLMProvider::new(vec![
        Ok(create_test_response(
            "read_files",
            serde_json::json!({
                "project": "test",
                "paths": ["test.txt"]
            }),
            "Reading with correct parameters",
        )),
        // Simulate LLM sending invalid params
        Ok(LLMResponse {
            content: vec![ContentBlock::ToolUse {
                id: "test-id".to_string(),
                name: "read_files".to_string(),
                input: serde_json::json!({
                    // Missing required 'paths' parameter
                    "wrong_param": "value"
                }),
            }],
            usage: Usage::zero(),
        }),
    ]);
    let mock_llm_ref = mock_llm.clone();

    let mut agent = Agent::new(
        Box::new(mock_llm),
        ToolMode::Native,
        Box::new(MockProjectManager::new()),
        Box::new(create_command_executor_mock()),
        Box::new(MockUI::default()),
        Box::new(MockStatePersistence::new()),
        Some(PathBuf::from("./test_path")),
    );

    agent.start_with_task("Test task".to_string()).await?;

    let requests = mock_llm_ref.get_requests();

    // Should see three requests:
    // 1. Failed parse
    // 2. Corrected ReadFiles
    // 3. CompleteTask
    assert_eq!(requests.len(), 3);

    // Check error was communicated to LLM
    let error_request = &requests[1];
    assert!(error_request.messages.len() >= 2); // May have changed with the new implementation

    // Find the error message
    let error_message = error_request.messages.iter().find_map(|msg| {
        if let MessageContent::Text(content) = &msg.content {
            if content.contains("Error") || content.contains("error") || content.contains("missing")
            {
                Some(content)
            } else {
                None
            }
        } else {
            None
        }
    });

    assert!(
        error_message.is_some(),
        "Error message not found in response"
    );
    assert!(
        error_message.unwrap().contains("parameter")
            || error_message.unwrap().contains("parameters")
    );
    assert!(error_message.unwrap().contains("read_files"));

    Ok(())
}
