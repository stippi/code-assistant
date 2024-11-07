use super::*;
use crate::llm::{types::*, LLMProvider, LLMRequest};
use crate::types::*;
use crate::ui::{UIError, UIMessage, UserInterface};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// Mock LLM Provider
#[derive(Default, Clone)]
struct MockLLMProvider {
    requests: Arc<Mutex<Vec<LLMRequest>>>,
    responses: Arc<Mutex<Vec<Result<LLMResponse, anyhow::Error>>>>,
}

impl MockLLMProvider {
    fn new(responses: Vec<Result<LLMResponse, anyhow::Error>>) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(responses)),
        }
    }
}

#[async_trait]
impl LLMProvider for MockLLMProvider {
    async fn send_message(&self, request: LLMRequest) -> Result<LLMResponse, anyhow::Error> {
        self.requests.lock().unwrap().push(request);
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
    pub fn new(files: HashMap<PathBuf, String>, file_tree: Option<FileTreeEntry>) -> Self {
        Self {
            files: Arc::new(Mutex::new(files)),
            file_tree: Arc::new(Mutex::new(file_tree)),
        }
    }
}

impl CodeExplorer for MockExplorer {
    fn root_dir(&self) -> PathBuf {
        PathBuf::from("root")
    }

    fn read_file(&self, path: &PathBuf) -> Result<String, anyhow::Error> {
        self.files
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("File not found: {}", path.display()))
    }

    fn create_initial_tree(&self, _max_depth: usize) -> Result<FileTreeEntry, anyhow::Error> {
        self.file_tree
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No file tree configured"))
    }

    fn list_files(
        &self,
        path: &PathBuf,
        _max_depth: Option<usize>,
    ) -> Result<FileTreeEntry, anyhow::Error> {
        // Return just an error for now
        Err(anyhow::anyhow!("Path not found: {}", path.display()))
    }

    fn apply_updates(&self, path: &Path, updates: &[FileUpdate]) -> Result<String, anyhow::Error> {
        let mut files = self.files.lock().unwrap();

        let content = files
            .get(path)
            .ok_or_else(|| anyhow::anyhow!("File not found: {}", path.display()))?;

        let lines: Vec<&str> = content.lines().collect();
        let mut result = String::new();

        // Validate updates
        for update in updates {
            if update.start_line == 0 || update.end_line == 0 {
                return Err(anyhow::anyhow!("Line numbers must start at 1"));
            }
            if update.start_line > update.end_line {
                return Err(anyhow::anyhow!(
                    "Start line must not be greater than end line"
                ));
            }
            if update.end_line > lines.len() {
                return Err(anyhow::anyhow!(
                    "End line {} exceeds file length {}",
                    update.end_line,
                    lines.len()
                ));
            }
        }

        // Apply updates
        let mut current_line = 1;
        for update in updates {
            // Add lines before the update
            while current_line < update.start_line {
                result.push_str(lines[current_line - 1]);
                result.push('\n');
                current_line += 1;
            }

            // Add the update
            result.push_str(&update.new_content);
            if !update.new_content.ends_with('\n') {
                result.push('\n');
            }

            current_line = update.end_line + 1;
        }

        // Add remaining lines
        while current_line <= lines.len() {
            result.push_str(lines[current_line - 1]);
            result.push('\n');
            current_line += 1;
        }

        // Update the stored content
        files.insert(path.to_path_buf(), result.clone());

        Ok(result)
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

fn create_explorer_mock() -> MockExplorer {
    let mut files = HashMap::new();
    files.insert(
        PathBuf::from("test.txt"),
        "line 1\nline 2\nline 3\n".to_string(),
    );

    let file_tree = Some(FileTreeEntry {
        name: "root".to_string(),
        entry_type: FileSystemEntryType::Directory,
        children: HashMap::new(),
        is_expanded: true,
    });

    MockExplorer::new(files, file_tree)
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

    let mut agent = Agent::new(
        Box::new(mock_llm),
        Box::new(create_explorer_mock()),
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

    let mock_llm = MockLLMProvider::new(vec![Ok(create_test_response(
        Tool::AskUser {
            question: test_question.to_string(),
        },
        "Need to ask user a question",
        true,
    ))]);

    let mock_ui = MockUI::new(vec![Ok(test_answer.to_string())]);

    let mut agent = Agent::new(
        Box::new(mock_llm),
        Box::new(create_explorer_mock()),
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

#[tokio::test]
async fn test_agent_read_files() -> Result<(), anyhow::Error> {
    // Test success case
    let mock_llm = MockLLMProvider::new(vec![
        Ok(create_test_response(
            Tool::ReadFiles {
                paths: vec![PathBuf::from("test.txt")],
            },
            "Reading test file",
            false,
        )),
        Ok(create_test_response(
            Tool::MessageUser {
                message: (String::from("Done")),
            },
            "Dummy reason",
            true,
        )),
    ]);
    // Obtain a reference to the mock_llm before handing ownership to the agent
    let mock_llm_ref = mock_llm.clone();

    let mut agent = Agent::new(
        Box::new(mock_llm),
        Box::new(create_explorer_mock()),
        Box::new(MockUI::default()),
    );

    // Run the agent
    agent.start("Test task".to_string()).await?;

    // Verify the file is displayed in the working memory
    let locked_requests = mock_llm_ref.requests.lock().unwrap();
    for request in locked_requests.iter() {
        println!("Request: {:#?}", request);
    }

    // if let LLMRequest(req) = &locked_requests[1] {
    //     // First message is about creating repository structure
    //     println!("Request: {:#?}", req);
    // }

    Ok(())
}
