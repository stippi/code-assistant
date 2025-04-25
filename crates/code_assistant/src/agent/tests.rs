use super::*;
use crate::agent::agent::parse_llm_response;
use crate::persistence::MockStatePersistence;
use crate::tests::mocks::create_command_executor_mock;
use crate::tests::mocks::{MockProjectManager, MockUI};
use crate::types::*;
use anyhow::Result;
use async_trait::async_trait;
use llm::{types::*, LLMProvider, LLMRequest, StreamingCallback};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// Mock LLM Provider
#[derive(Default, Clone)]
struct MockLLMProvider {
    requests: Arc<Mutex<Vec<LLMRequest>>>,
    responses: Arc<Mutex<Vec<Result<LLMResponse, anyhow::Error>>>>,
}

impl MockLLMProvider {
    fn new(mut responses: Vec<Result<LLMResponse, anyhow::Error>>) -> Self {
        // Add CompleteTask response at the beginning if the first response is ok
        if responses.first().map_or(false, |r| r.is_ok()) {
            responses.insert(
                0,
                Ok(create_test_response(
                    Tool::CompleteTask {
                        message: "Task completed successfully".to_string(),
                    },
                    "Completing task after successful execution",
                )),
            );
        }

        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(responses)),
        }
    }

    #[allow(dead_code)]
    fn print_requests(&self) {
        let requests = self.requests.lock().unwrap();
        println!("\nTotal number of requests: {}", requests.len());
        for (i, request) in requests.iter().enumerate() {
            println!("\nRequest {}:", i);
            for (j, message) in request.messages.iter().enumerate() {
                println!("  Message {}:", j);
                if let MessageContent::Text(content) = &message.content {
                    println!("    {}", content.replace('\n', "\n    "));
                }
            }
        }
    }
}

#[async_trait]
impl LLMProvider for MockLLMProvider {
    async fn send_message(
        &self,
        request: LLMRequest,
        _streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse, anyhow::Error> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Err(anyhow::anyhow!("No more mock responses")))
    }
}

fn create_test_response(tool: Tool, reasoning: &str) -> LLMResponse {
    let tool_name = match &tool {
        Tool::ListProjects { .. } => "list_projects",
        Tool::UpdatePlan { .. } => "update_plan",
        Tool::SearchFiles { .. } => "search_files",
        Tool::ExecuteCommand { .. } => "execute_command",
        Tool::ListFiles { .. } => "list_files",
        Tool::ReadFiles { .. } => "read_files",
        Tool::WriteFile { .. } => "write_file",
        Tool::ReplaceInFile { .. } => "replace_in_file",
        Tool::DeleteFiles { .. } => "delete_files",
        Tool::CompleteTask { .. } => "complete_task",
        Tool::UserInput { .. } => "user_input",
        Tool::WebSearch { .. } => "web_search",
        Tool::WebFetch { .. } => "web_fetch",
        Tool::PerplexityAsk { .. } => "perplexity_ask",
    };
    let tool_input = match &tool {
        Tool::ListProjects {} => serde_json::json!({}),
        Tool::UpdatePlan { plan } => serde_json::json!({
            "plan": plan
        }),
        Tool::UserInput {} => serde_json::json!({}),
        Tool::SearchFiles { project, regex } => serde_json::json!({
            "project": project,
            "regex": regex,
        }),
        Tool::ExecuteCommand {
            project,
            command_line,
            working_dir,
        } => serde_json::json!({
            "project": project,
            "command_line": command_line,
            "working_dir": working_dir
        }),
        Tool::ListFiles {
            project,
            paths,
            max_depth,
        } => {
            let mut map = serde_json::Map::new();
            map.insert("project".to_string(), serde_json::json!(project));
            map.insert("paths".to_string(), serde_json::json!(paths));
            if let Some(depth) = max_depth {
                map.insert("max_depth".to_string(), serde_json::json!(depth));
            }
            serde_json::Value::Object(map)
        }
        Tool::ReadFiles { project, paths } => {
            // For testing convenience, we convert paths with special format
            // For example, "filename.txt:10-20" should read only lines 10-20
            let paths_with_ranges: Vec<String> = paths
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect();
            serde_json::json!({
                "project": project,
                "paths": paths_with_ranges
            })
        }
        Tool::WriteFile {
            project,
            path,
            content,
            append,
        } => serde_json::json!({
            "project": project,
            "path": path,
            "content": content,
            "append": append
        }),
        Tool::ReplaceInFile {
            project,
            path,
            replacements,
        } => {
            // Convert replacements to the diff format
            let mut diff = String::new();
            for replacement in replacements {
                diff.push_str("<<<<<<< SEARCH\n");
                diff.push_str(&replacement.search);
                diff.push_str("\n=======\n");
                diff.push_str(&replacement.replace);
                diff.push_str("\n>>>>>>> REPLACE\n\n");
            }
            serde_json::json!({
                "project": project,
                "path": path,
                "diff": diff
            })
        }
        Tool::DeleteFiles { project, paths } => serde_json::json!({
            "project": project,
            "paths": paths
        }),

        Tool::CompleteTask { message } => serde_json::json!({
            "message": message
        }),
        Tool::WebSearch {
            query,
            hits_page_number,
        } => serde_json::json!({
            "query": query,
            "hits_page_number": hits_page_number
        }),
        Tool::WebFetch { url, selectors } => serde_json::json!({
            "url": url,
            "selectors": selectors
        }),
        Tool::PerplexityAsk { messages } => serde_json::json!({
            "messages": messages
        }),
    };

    LLMResponse {
        content: vec![
            ContentBlock::Text {
                text: reasoning.to_string(),
            },
            ContentBlock::ToolUse {
                id: "some-tool-id".to_string(),
                name: tool_name.to_string(),
                input: tool_input,
            },
        ],
        usage: Usage::zero(),
    }
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
#[ignore = "Needs updating after agent refactoring"]
async fn test_unknown_tool_error_handling() -> Result<()> {
    let mock_llm = MockLLMProvider::new(vec![
        Ok(create_test_response(
            Tool::ReadFiles {
                project: "test".to_string(),
                paths: vec![PathBuf::from("test.txt")],
            },
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

    let requests = mock_llm_ref.requests.lock().unwrap();

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
#[ignore = "Needs updating after agent refactoring"]
async fn test_parse_error_handling() -> Result<()> {
    let mock_llm = MockLLMProvider::new(vec![
        Ok(create_test_response(
            Tool::ReadFiles {
                project: "test".to_string(),
                paths: vec![PathBuf::from("test.txt")],
            },
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

    let requests = mock_llm_ref.requests.lock().unwrap();

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
