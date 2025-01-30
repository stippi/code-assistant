use super::*;
use crate::agent::agent::parse_llm_response;
use crate::llm::{types::*, LLMProvider, LLMRequest};
use crate::persistence::MockStatePersistence;
use crate::types::*;
use crate::ui::{UIError, UIMessage, UserInterface};
use crate::utils::{CommandExecutor, CommandOutput};
use agent::ToolMode;
use anyhow::Result;
use async_trait::async_trait;
use regex::RegexBuilder;
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

    fn write_file(&self, path: &PathBuf, content: &String) -> Result<()> {
        let mut files = self.files.lock().unwrap();
        files.insert(path.to_path_buf(), content.clone());
        Ok(())
    }

    fn delete_file(&self, path: &PathBuf) -> Result<()> {
        let mut files = self.files.lock().unwrap();
        files.remove(path);
        Ok(())
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

    fn apply_replacements(&self, path: &Path, replacements: &[FileReplacement]) -> Result<String> {
        let mut files = self.files.lock().unwrap();

        let content = files
            .get(path)
            .ok_or_else(|| anyhow::anyhow!("File not found: {}", path.display()))?
            .clone();

        let updated_content = crate::utils::apply_replacements(&content, replacements)?;

        // Update the stored content
        files.insert(path.to_path_buf(), updated_content.clone());

        Ok(updated_content)
    }

    fn search(
        &self,
        path: &Path,
        options: SearchOptions,
    ) -> Result<Vec<SearchResult>, anyhow::Error> {
        let files = self.files.lock().unwrap();
        let max_results = options.max_results.unwrap_or(usize::MAX);
        let mut results = Vec::new();

        // Create regex based on search mode
        let regex = match options.mode {
            SearchMode::Exact => {
                // For exact search, escape regex special characters and optionally add word boundaries
                let pattern = if options.whole_words {
                    format!(r"\b{}\b", regex::escape(&options.query))
                } else {
                    regex::escape(&options.query)
                };
                RegexBuilder::new(&pattern)
                    .case_insensitive(!options.case_sensitive)
                    .build()?
            }
            SearchMode::Regex => {
                // For regex search, optionally add word boundaries to user's pattern
                let pattern = if options.whole_words {
                    format!(r"\b{}\b", options.query)
                } else {
                    options.query.clone()
                };
                RegexBuilder::new(&pattern)
                    .case_insensitive(!options.case_sensitive)
                    .build()?
            }
        };

        for (file_path, content) in files.iter() {
            // Only search files under the specified path
            if !file_path.starts_with(path) {
                continue;
            }

            for (line_idx, line) in content.lines().enumerate() {
                let matches: Vec<_> = regex.find_iter(line).collect();
                if !matches.is_empty() {
                    results.push(SearchResult {
                        file: file_path.clone(),
                        line_number: line_idx + 1,
                        line_content: line.to_string(),
                        match_ranges: matches.iter().map(|m| (m.start(), m.end())).collect(),
                    });

                    if results.len() >= max_results {
                        return Ok(results);
                    }
                }
            }
        }

        Ok(results)
    }
}

// Helper function to create a test response
fn create_test_response(tool: Tool, reasoning: &str) -> LLMResponse {
    let tool_name = match &tool {
        Tool::SearchFiles { .. } => "search_files",
        Tool::ExecuteCommand { .. } => "execute_command",
        Tool::ListFiles { .. } => "list_files",
        Tool::ReadFiles { .. } => "read_files",
        Tool::WriteFile { .. } => "write_file",
        Tool::ReplaceInFile { .. } => "replace_in_file",
        Tool::DeleteFiles { .. } => "delete_files",
        Tool::Summarize { .. } => "summarize",
        Tool::AskUser { .. } => "ask_user",
        Tool::MessageUser { .. } => "message_user",
        Tool::CompleteTask { .. } => "complete_task",
    };
    let tool_input = match &tool {
        Tool::SearchFiles {
            query,
            path,
            case_sensitive,
            whole_words,
            regex_mode,
            max_results,
        } => serde_json::json!({
            "query": query,
            "path": path,
            "case_sensitive": case_sensitive,
            "whole_words": whole_words,
            "regex_mode": regex_mode,
            "max_results": max_results
        }),
        Tool::ExecuteCommand {
            command_line,
            working_dir,
        } => serde_json::json!({
            "command_line": command_line,
            "working_dir": working_dir
        }),
        Tool::ListFiles { paths, max_depth } => {
            let mut map = serde_json::Map::new();
            map.insert("paths".to_string(), serde_json::json!(paths));
            if let Some(depth) = max_depth {
                map.insert("max_depth".to_string(), serde_json::json!(depth));
            }
            serde_json::Value::Object(map)
        }
        Tool::ReadFiles { paths } => serde_json::json!({
            "paths": paths
        }),
        Tool::WriteFile { path, content } => serde_json::json!({
            "path": path,
            "content": content
        }),
        Tool::ReplaceInFile { path, replacements } => serde_json::json!({
            "path": path,
            "replacements": replacements
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
        Tool::CompleteTask { message } => serde_json::json!({
            "message": message
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

#[test]
fn test_mock_explorer_search() -> Result<(), anyhow::Error> {
    let mut files = HashMap::new();
    files.insert(
        PathBuf::from("./root/test1.txt"),
        "line 1\nline 2\nline 3\n".to_string(),
    );
    files.insert(
        PathBuf::from("./root/test2.txt"),
        "another line\nmatching line\n".to_string(),
    );
    files.insert(
        PathBuf::from("./root/subdir/test3.txt"),
        "subdir line\nmatching line\n".to_string(),
    );

    let explorer = MockExplorer::new(files, None);

    // Test basic search
    let results = explorer.search(
        &PathBuf::from("./root"),
        SearchOptions {
            query: "matching".to_string(),
            ..Default::default()
        },
    )?;
    assert_eq!(results.len(), 2);
    assert!(results.iter().any(|r| r.file.ends_with("test2.txt")));
    assert!(results.iter().any(|r| r.file.ends_with("test3.txt")));

    // Test case-sensitive search
    let results = explorer.search(
        &PathBuf::from("./root"),
        SearchOptions {
            query: "LINE".to_string(),
            case_sensitive: true,
            ..Default::default()
        },
    )?;
    assert_eq!(results.len(), 0); // Should find nothing with case-sensitive search

    // Test case-insensitive search
    let results = explorer.search(
        &PathBuf::from("./root"),
        SearchOptions {
            query: "LINE".to_string(),
            case_sensitive: false,
            ..Default::default()
        },
    )?;
    assert!(results.len() > 0); // Should find matches

    // Test whole word search
    let results = explorer.search(
        &PathBuf::from("./root"),
        SearchOptions {
            query: "line".to_string(),
            whole_words: true,
            ..Default::default()
        },
    )?;
    // When searching for whole words, matches should not be part of other words
    assert!(results.iter().all(|r| {
        let line = &r.line_content;
        // Check that "line" is not part of another word
        !line.contains("inline") && !line.contains("pipeline") && !line.contains("airline")
    }));

    // Test regex mode
    let results = explorer.search(
        &PathBuf::from("./root"),
        SearchOptions {
            query: r"line \d".to_string(),
            mode: SearchMode::Regex,
            ..Default::default()
        },
    )?;
    assert!(results.iter().any(|r| r.line_content.contains("line 1")));

    // Test regex search
    let results = explorer.search(
        &PathBuf::from("./root"),
        SearchOptions {
            query: r"line \d+".to_string(), // Match "line" followed by numbers
            mode: SearchMode::Regex,
            ..Default::default()
        },
    )?;
    assert!(results.iter().any(|r| r.line_content.contains("line 1")));

    // Test with max_results
    let results = explorer.search(
        &PathBuf::from("./root"),
        SearchOptions {
            query: "line".to_string(),
            max_results: Some(2),
            ..Default::default()
        },
    )?;
    assert_eq!(results.len(), 2);

    // Test search in subdirectory
    let results = explorer.search(
        &PathBuf::from("./root/subdir"),
        SearchOptions {
            query: "subdir".to_string(),
            ..Default::default()
        },
    )?;
    assert_eq!(results.len(), 1);
    assert!(results[0].file.ends_with("test3.txt"));

    // Test search with no matches
    let results = explorer.search(
        &PathBuf::from("./root"),
        SearchOptions {
            query: "nonexistent".to_string(),
            ..Default::default()
        },
    )?;
    assert_eq!(results.len(), 0);

    Ok(())
}

#[tokio::test]
async fn test_agent_start_with_message() -> Result<(), anyhow::Error> {
    // Prepare test data
    let test_message = "Test message for user";
    let test_reasoning = "Dummy reason";
    let tool = Tool::MessageUser {
        message: test_message.to_string(),
    };

    let mock_llm = MockLLMProvider::new(vec![Ok(create_test_response(tool, test_reasoning))]);

    let mock_ui = MockUI::default();

    let mut agent = Agent::new(
        Box::new(mock_llm),
        ToolMode::Native,
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

    // First message is about creating repository structure
    if let UIMessage::Reasoning(msg) = &messages[1] {
        assert!(msg.contains(test_reasoning));
    } else {
        panic!("Expected UIMessage::Reasoning");
    }

    if let UIMessage::Action(msg) = &messages[2] {
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
        ToolMode::Native,
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
        ToolMode::Native,
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
        assert!(
            content.contains(
                "Loaded files and their contents:\n\n-----test.txt:\nline 1\nline 2\nline 3\n"
            ),
            "File content not found in working memory message:\n{}",
            content
        );
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
        ToolMode::Native,
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

#[test]
fn test_flexible_xml_parsing() -> Result<()> {
    // Initialize logging for this test
    let _ = tracing_subscriber::fmt()
        .with_env_filter("code_assistant=debug")
        .with_test_writer()
        .try_init();

    let response = LLMResponse {
        content: vec![ContentBlock::Text {
            text: r#"I will search for TODO comments in the code.

<tool:search_files>
<param:query>TODO & FIXME <html></param:query>
<param:path>src/</param:path>
<param:case_sensitive>true</param:case_sensitive>
<param:max_results>
    50
</param:max_results>
</tool:search_files>"#
                .to_string(),
        }],
    };

    let actions = parse_llm_response(&response)?;
    assert_eq!(actions.len(), 1);
    assert!(actions[0].reasoning.contains("search for TODO comments"));

    if let Tool::SearchFiles {
        query, max_results, ..
    } = &actions[0].tool
    {
        assert_eq!(query, "TODO & FIXME <html>"); // Notice the & character is allowed and also tags
        assert_eq!(*max_results, Some(50));
    } else {
        panic!("Expected Search tool");
    }

    Ok(())
}

#[test]
fn test_replacement_xml_parsing() -> Result<()> {
    let response = LLMResponse {
        content: vec![ContentBlock::Text {
            text: r#"I will fix the code formatting.

<tool:replace_in_file>
<param:path>src/main.rs</param:path>
<param:diff>
<<<<<<< SEARCH
function test(){
  console.log("messy");
}
=======
function test() {
    console.log("clean");
}
>>>>>>> REPLACE

<<<<<<< SEARCH
const x=42
=======
const x = 42;
>>>>>>> REPLACE
</param:diff>
</tool:replace_in_file>"#
                .to_string(),
        }],
    };

    let actions = parse_llm_response(&response)?;
    assert_eq!(actions.len(), 1);
    assert!(actions[0].reasoning.contains("fix the code formatting"));

    if let Tool::ReplaceInFile { path, replacements } = &actions[0].tool {
        assert_eq!(path, &PathBuf::from("src/main.rs"));
        assert_eq!(replacements.len(), 2);
        assert_eq!(
            replacements[0].search,
            "function test(){\n  console.log(\"messy\");\n}"
        );
        assert_eq!(
            replacements[0].replace,
            "function test() {\n    console.log(\"clean\");\n}"
        );
        assert_eq!(replacements[1].search, "const x=42");
        assert_eq!(replacements[1].replace, "const x = 42;");
    } else {
        panic!("Expected ReplaceInFile tool");
    }

    Ok(())
}

#[test]
fn test_apply_replacements() -> Result<(), anyhow::Error> {
    let mut files = HashMap::new();
    files.insert(
        PathBuf::from("./root/test.txt"),
        "Hello World\nThis is a test\nGoodbye".to_string(),
    );

    let explorer = MockExplorer::new(files, None);

    let replacements = vec![
        FileReplacement {
            search: "Hello World".to_string(),
            replace: "Hi there".to_string(),
        },
        FileReplacement {
            search: "Goodbye".to_string(),
            replace: "See you".to_string(),
        },
    ];

    let result = explorer.apply_replacements(&PathBuf::from("./root/test.txt"), &replacements)?;

    assert_eq!(result, "Hi there\nThis is a test\nSee you");
    Ok(())
}
