use super::*;
use crate::llm::{types::*, LLMProvider, LLMRequest};
use crate::persistence::MockStatePersistence;
use crate::types::*;
use crate::ui::{UIError, UIMessage, UserInterface};
use crate::utils::{CommandExecutor, CommandOutput};
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
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

    // // Helper method for tests that need specific completion handling
    // fn new_with_custom_completion(
    //     mut responses: Vec<Result<LLMResponse, anyhow::Error>>,
    //     completion_message: Option<String>,
    // ) -> Self {
    //     if let Some(msg) = completion_message {
    //         responses.push(Ok(create_test_response(
    //             Tool::CompleteTask { message: msg },
    //             "Custom completion message",
    //         )));
    //     }

    //     Self {
    //         requests: Arc::new(Mutex::new(Vec::new())),
    //         responses: Arc::new(Mutex::new(responses)),
    //     }
    // }
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

// Mock CommandExecutor
#[derive(Clone)]
struct MockCommandExecutor {
    responses: Arc<Mutex<Vec<Result<CommandOutput, anyhow::Error>>>>,
    calls: Arc<AtomicUsize>,
    captured_commands: Arc<Mutex<Vec<(String, Option<PathBuf>)>>>,
}

impl MockCommandExecutor {
    fn new(responses: Vec<Result<CommandOutput, anyhow::Error>>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            calls: Arc::new(AtomicUsize::new(0)),
            captured_commands: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn get_captured_commands(&self) -> Vec<(String, Option<PathBuf>)> {
        self.captured_commands.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl CommandExecutor for MockCommandExecutor {
    async fn execute(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
    ) -> Result<CommandOutput> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.captured_commands
            .lock()
            .unwrap()
            .push((command_line.to_string(), working_dir.cloned()));

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
        PathBuf::from("./root")
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
            .ok_or_else(|| anyhow::anyhow!("File not found: {}", path.display()))?
            .clone();

        let updated_content = crate::utils::apply_content_updates(&content, updates)?;

        // Update the stored content
        files.insert(path.to_path_buf(), updated_content.clone());

        Ok(updated_content)
    }
}

// Helper function to create a test response
fn create_test_response(tool: Tool, reasoning: &str) -> LLMResponse {
    let response = serde_json::json!({
        "reasoning": reasoning,
        "tool": {
            "name": match &tool {
                Tool::ListFiles { .. } => "ListFiles",
                Tool::ReadFiles { .. } => "ReadFiles",
                Tool::WriteFile { .. } => "WriteFile",
                Tool::UpdateFile { .. } => "UpdateFile",
                Tool::DeleteFiles { .. } => "DeleteFiles",
                Tool::Summarize { .. } => "Summarize",
                Tool::AskUser { .. } => "AskUser",
                Tool::MessageUser { .. } => "MessageUser",
                Tool::ExecuteCommand { .. } => "ExecuteCommand",
                Tool::CompleteTask { .. } => "CompleteTask",
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
                Tool::DeleteFiles { paths } => serde_json::json!({
                    "paths": paths
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
                Tool::ExecuteCommand { command_line, working_dir } => serde_json::json!({
                    "command_line": command_line,
                    "working_dir": working_dir
                }),
                Tool::CompleteTask { message } => serde_json::json!({
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
        PathBuf::from("./root/test.txt"),
        "line 1\nline 2\nline 3\n".to_string(),
    );

    let file_tree = Some(FileTreeEntry {
        name: "./root".to_string(),
        entry_type: FileSystemEntryType::Directory,
        children: HashMap::new(),
        is_expanded: true,
    });

    MockExplorer::new(files, file_tree)
}

fn create_command_executor_mock() -> MockCommandExecutor {
    MockCommandExecutor::new(vec![])
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
    ))]);

    let mock_ui = MockUI::default();

    let mut agent = Agent::new(
        Box::new(mock_llm),
        Box::new(create_explorer_mock()),
        Box::new(create_command_executor_mock()),
        Box::new(mock_ui.clone()),
        Box::new(MockStatePersistence::new()),
    );

    // Run the agent
    agent.start_with_task("Test task".to_string()).await?;

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
    ))]);

    let mock_ui = MockUI::new(vec![Ok(test_answer.to_string())]);

    let mut agent = Agent::new(
        Box::new(mock_llm),
        Box::new(create_explorer_mock()),
        Box::new(create_command_executor_mock()),
        Box::new(mock_ui.clone()),
        Box::new(MockStatePersistence::new()),
    );

    // Run the agent
    agent.start_with_task("Test task".to_string()).await?;

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
        // Responses in reverse order
        Ok(create_test_response(
            Tool::MessageUser {
                message: (String::from("Done")),
            },
            "Dummy reason",
        )),
        Ok(create_test_response(
            Tool::ReadFiles {
                paths: vec![PathBuf::from("test.txt")],
            },
            "Reading test file",
        )),
    ]);
    // Obtain a reference to the mock_llm before handing ownership to the agent
    let mock_llm_ref = mock_llm.clone();

    let mut agent = Agent::new(
        Box::new(mock_llm),
        Box::new(create_explorer_mock()),
        Box::new(create_command_executor_mock()),
        Box::new(MockUI::default()),
        Box::new(MockStatePersistence::new()),
    );

    // Run the agent
    agent.start_with_task("Test task".to_string()).await?;

    // Verify the file is displayed in the working memory of the second request
    let locked_requests = mock_llm_ref.requests.lock().unwrap();
    let second_request = &locked_requests[1];

    if let MessageContent::Text(content) = &second_request.messages[0].content {
        assert!(content.contains(
            "Loaded files and their contents:\n  -----test.txt:\n   1 | line 1\n   2 | line 2\n   3 | line 3\n"
        ), "File content not found in working memory message:\n{}", content);
    } else {
        panic!("Expected text content in message");
    }

    Ok(())
}

#[tokio::test]
async fn test_execute_command() -> Result<()> {
    let test_output = CommandOutput {
        success: true,
        stdout: "command output".to_string(),
        stderr: "".to_string(),
    };

    let mock_command_executor = MockCommandExecutor::new(vec![Ok(test_output)]);
    let mock_command_executor_ref = mock_command_executor.clone();

    let mock_llm = MockLLMProvider::new(vec![Ok(create_test_response(
        Tool::ExecuteCommand {
            command_line: "test command".to_string(),
            working_dir: None,
        },
        "Testing command execution",
    ))]);

    let mut agent = Agent::new(
        Box::new(mock_llm),
        Box::new(create_explorer_mock()),
        Box::new(mock_command_executor),
        Box::new(MockUI::default()),
        Box::new(MockStatePersistence::new()),
    );

    // Run the agent
    agent.start_with_task("Test task".to_string()).await?;

    // Verify number of calls and command parameters
    assert_eq!(mock_command_executor_ref.calls.load(Ordering::Relaxed), 1);

    let captured_commands = mock_command_executor_ref.get_captured_commands();
    assert_eq!(captured_commands.len(), 1);
    assert_eq!(captured_commands[0].0, "test command");
    assert_eq!(captured_commands[0].1, None);

    Ok(())
}
