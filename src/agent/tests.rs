use super::*;
use crate::llm::{types::*, LLMProvider, LLMRequest};
use crate::types::*;
use crate::ui::{UIError, UIMessage, UserInterface};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tempfile;

// Mock LLM Provider
#[derive(Default)]
struct MockLLMProvider {
    responses: Arc<Mutex<Vec<Result<LLMResponse, anyhow::Error>>>>,
}

impl MockLLMProvider {
    fn new(responses: Vec<Result<LLMResponse, anyhow::Error>>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
        }
    }
}

#[async_trait]
impl LLMProvider for MockLLMProvider {
    async fn send_message(&self, _request: LLMRequest) -> Result<LLMResponse, anyhow::Error> {
        self.responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Err(anyhow::anyhow!("No more mock responses")))
    }
}

// Mock UI
#[derive(Default, Clone)]
struct MockUI {
    messages: Arc<Mutex<Vec<UIMessage>>>,
    responses: Arc<Mutex<Vec<Result<String, UIError>>>>,
}

impl MockUI {
    fn new(responses: Vec<Result<String, UIError>>) -> Self {
        Self {
            messages: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(responses)),
        }
    }

    fn get_messages(&self) -> Vec<UIMessage> {
        self.messages.lock().unwrap().clone()
    }
}

#[async_trait]
impl UserInterface for MockUI {
    async fn display(&self, message: UIMessage) -> Result<(), UIError> {
        self.messages.lock().unwrap().push(message);
        Ok(())
    }

    async fn get_input(&self, _prompt: &str) -> Result<String, UIError> {
        self.responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Err(UIError::IOError(std::io::Error::new(
                std::io::ErrorKind::Other,
                "No more mock responses",
            ))))
    }
}

// Mock Explorer
#[derive(Default)]
struct MockExplorer {
    files: Arc<Mutex<HashMap<PathBuf, String>>>,
    file_tree: Arc<Mutex<Option<FileTreeEntry>>>,
}

impl MockExplorer {
    fn new(files: HashMap<PathBuf, String>, file_tree: Option<FileTreeEntry>) -> Self {
        Self {
            files: Arc::new(Mutex::new(files)),
            file_tree: Arc::new(Mutex::new(file_tree)),
        }
    }
}

// Helper function to create a test response
fn create_test_response(tool: Tool, reasoning: &str, task_completed: bool) -> LLMResponse {
    let response = serde_json::json!({
        "reasoning": reasoning,
        "task_completed": task_completed,
        "tool": {
            "name": match &tool {
                Tool::ListFiles { .. } => "ListFiles",
                Tool::ReadFiles { .. } => "ReadFiles",
                Tool::WriteFile { .. } => "WriteFile",
                Tool::UpdateFile { .. } => "UpdateFile",
                Tool::Summarize { .. } => "Summarize",
                Tool::AskUser { .. } => "AskUser",
                Tool::MessageUser { .. } => "MessageUser",
            },
            "params": match &tool {
                Tool::ListFiles { paths, max_depth } => {
                    let mut map = serde_json::Map::new();
                    map.insert("paths".to_string(), serde_json::json!(paths));
                    if let Some(depth) = max_depth {
                        map.insert("max_depth".to_string(), serde_json::json!(depth));
                    }
                    serde_json::Value::Object(map)
                },
                Tool::ReadFiles { paths } => serde_json::json!({
                    "paths": paths
                }),
                Tool::WriteFile { path, content } => serde_json::json!({
                    "path": path,
                    "content": content
                }),
                Tool::UpdateFile { path, updates } => serde_json::json!({
                    "path": path,
                    "updates": updates
                }),
                Tool::Summarize { files } => serde_json::json!({
                    "files": files.iter().map(|(path, summary)| {
                        serde_json::json!({
                            "path": path,
                            "summary": summary
                        })
                    }).collect::<Vec<_>>()
                }),
                Tool::AskUser { question } => serde_json::json!({
                    "question": question
                }),
                Tool::MessageUser { message } => serde_json::json!({
                    "message": message
                }),
            }
        }
    });

    LLMResponse {
        content: vec![ContentBlock::Text {
            text: response.to_string(),
        }],
    }
}

#[tokio::test]
async fn test_agent_start_with_message() -> Result<(), anyhow::Error> {
    // Prepare test data
    let test_message = "Test message for user";
    let tool = Tool::MessageUser {
        message: test_message.to_string(),
    };

    let mock_llm = MockLLMProvider::new(vec![Ok(create_test_response(
        tool,
        "Testing message to user",
        true,
    ))]);

    let mock_ui = MockUI::default();
    let test_dir = tempfile::tempdir()?;

    let mut agent = Agent::new(
        Box::new(mock_llm),
        test_dir.path().to_path_buf(),
        Box::new(mock_ui.clone()),
    );

    // Run the agent
    agent.start("Test task".to_string()).await?;

    // Verify the message was displayed
    let messages = mock_ui.get_messages();
    assert!(!messages.is_empty());

    if let UIMessage::Action(msg) = &messages[1] {
        // First message is about creating repository structure
        assert!(msg.contains(test_message));
    } else {
        panic!("Expected UIMessage::Action");
    }

    Ok(())
}

#[tokio::test]
async fn test_agent_ask_user() -> Result<(), anyhow::Error> {
    // Prepare test data
    let test_question = "Test question?";
    let test_answer = "Test answer";

    let tool = Tool::AskUser {
        question: test_question.to_string(),
    };

    let mock_llm = MockLLMProvider::new(vec![Ok(create_test_response(
        tool,
        "Need to ask user a question",
        true,
    ))]);

    let mock_ui = MockUI::new(vec![Ok(test_answer.to_string())]);
    let test_dir = tempfile::tempdir()?;

    let mut agent = Agent::new(
        Box::new(mock_llm),
        test_dir.path().to_path_buf(),
        Box::new(mock_ui.clone()),
    );

    // Run the agent
    agent.start("Test task".to_string()).await?;

    // Verify the question was asked
    let messages = mock_ui.get_messages();
    assert!(messages.iter().any(|msg| match msg {
        UIMessage::Question(q) => q == test_question,
        _ => false,
    }));

    Ok(())
}
