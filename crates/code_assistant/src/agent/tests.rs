use super::*;
use crate::agent::agent::parse_llm_response;
use crate::agent::AgentMode;
use crate::config::ProjectManager;
use crate::persistence::MockStatePersistence;
use crate::types::*;
use crate::ui::{ToolStatus, UIError, UIMessage, UserInterface};
use crate::utils::{CommandExecutor, CommandOutput};
use anyhow::Result;
use async_trait::async_trait;
use llm::{types::*, LLMProvider, LLMRequest, StreamingCallback};
use regex::RegexBuilder;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

// Mock ProjectManager for tests
#[derive(Default)]
struct MockProjectManager {
    explorers: HashMap<String, MockExplorer>,
    projects: HashMap<String, Project>,
}

impl MockProjectManager {
    fn new() -> Self {
        let empty = Self {
            explorers: HashMap::new(),
            projects: HashMap::new(),
        };
        // Add default project
        empty.with_project("test", PathBuf::from("./root"), create_explorer_mock())
    }

    // Helper to add a custom project and explorer
    fn with_project(mut self, name: &str, path: PathBuf, explorer: MockExplorer) -> Self {
        self.projects.insert(name.to_string(), Project { path });
        self.explorers.insert(name.to_string(), explorer);
        self
    }
}

impl ProjectManager for MockProjectManager {
    fn add_temporary_project(&mut self, path: PathBuf) -> Result<String> {
        // Use a fixed name for testing
        let project_name = "temp_project".to_string();

        // Add the project
        self.projects
            .insert(project_name.clone(), Project { path: path.clone() });

        // Add a default explorer for it
        self.explorers
            .insert(project_name.clone(), create_explorer_mock());

        Ok(project_name)
    }

    fn get_projects(&self) -> Result<HashMap<String, Project>> {
        Ok(self.projects.clone())
    }

    fn get_project(&self, name: &str) -> Result<Option<Project>> {
        Ok(self.projects.get(name).cloned())
    }

    fn get_explorer_for_project(&self, name: &str) -> Result<Box<dyn CodeExplorer>> {
        match self.explorers.get(name) {
            Some(explorer) => Ok(Box::new(explorer.clone())),
            None => Err(anyhow::anyhow!("Project {} not found", name)),
        }
    }
}

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
    streaming: Arc<Mutex<Vec<String>>>,
    responses: Arc<Mutex<Vec<Result<String, UIError>>>>,
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

    fn display_fragment(&self, fragment: &crate::ui::DisplayFragment) -> Result<(), UIError> {
        // Convert the fragment to a string and add it to streaming collection
        match fragment {
            crate::ui::DisplayFragment::PlainText(text) => {
                self.streaming.lock().unwrap().push(text.clone());
            }
            crate::ui::DisplayFragment::ThinkingText(text) => {
                self.streaming.lock().unwrap().push(text.clone());
            }
            crate::ui::DisplayFragment::ToolName { name, .. } => {
                self.streaming.lock().unwrap().push(format!("\n• {}", name));
            }
            crate::ui::DisplayFragment::ToolParameter { name, value, .. } => {
                self.streaming
                    .lock()
                    .unwrap()
                    .push(format!("  {}: {}", name, value));
            }
            crate::ui::DisplayFragment::ToolEnd { .. } => {}
        }
        Ok(())
    }

    async fn update_memory(&self, _memory: &WorkingMemory) -> Result<(), UIError> {
        // Mock implementation does nothing with memory updates
        Ok(())
    }

    async fn update_tool_status(
        &self,
        _tool_id: &str,
        _status: ToolStatus,
        _message: Option<String>,
    ) -> Result<(), UIError> {
        // Mock implementation does nothing with the tool status
        Ok(())
    }

    async fn begin_llm_request(&self) -> Result<u64, UIError> {
        // For tests, return a fixed request ID
        Ok(42)
    }

    async fn end_llm_request(&self, _request_id: u64) -> Result<(), UIError> {
        // Mock implementation does nothing with request completion
        Ok(())
    }
}

// Mock Explorer
#[derive(Default, Clone)]
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

    fn read_file_range(
        &self,
        path: &PathBuf,
        start_line: Option<usize>,
        end_line: Option<usize>,
    ) -> Result<String, anyhow::Error> {
        let content = self.read_file(path)?;

        // If no line range is specified, return the whole file
        if start_line.is_none() && end_line.is_none() {
            return Ok(content);
        }

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();

        // Convert to 0-based indexing
        let start = start_line.map(|s| s.max(1) - 1).unwrap_or(0);
        let end = end_line
            .map(|e| (e.max(1) - 1).min(total_lines - 1))
            .unwrap_or(total_lines - 1);

        // Validate line range
        if start > end || start >= total_lines {
            return Err(anyhow::anyhow!(
                "Invalid line range: start={}, end={}, total_lines={}",
                start + 1, // Convert back to 1-based for the error message
                end + 1,   // Convert back to 1-based for the error message
                total_lines
            ));
        }

        // Extract the lines within the specified range
        let selected_content = lines[start..=end].join("\n");

        Ok(selected_content)
    }

    fn write_file(&self, path: &PathBuf, content: &String, append: bool) -> Result<()> {
        // Check parent directories
        for component in path.parent().unwrap_or(path).components() {
            let current = PathBuf::from(component.as_os_str());
            if let Some(_) = self.files.lock().unwrap().get(&current) {
                // If any parent is a file (has content), that's an error
                return Err(anyhow::anyhow!(
                    "Cannot create file: {} is a file",
                    current.display()
                ));
            }
        }

        let mut files = self.files.lock().unwrap();

        if append && files.contains_key(path) {
            // Append content to existing file
            if let Some(existing) = files.get_mut(path) {
                *existing = format!("{}{}", existing, content);
            }
        } else {
            // Write or overwrite file
            files.insert(path.to_path_buf(), content.clone());
        }

        Ok(())
    }

    fn delete_file(&self, path: &PathBuf) -> Result<()> {
        let mut files = self.files.lock().unwrap();
        files.remove(path);
        Ok(())
    }

    fn create_initial_tree(&mut self, _max_depth: usize) -> Result<FileTreeEntry, anyhow::Error> {
        self.file_tree
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No file tree configured"))
    }

    fn list_files(
        &mut self,
        path: &PathBuf,
        _max_depth: Option<usize>,
    ) -> Result<FileTreeEntry, anyhow::Error> {
        let file_tree = self.file_tree.lock().unwrap();
        let root = file_tree
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No file tree configured"))?;

        // Handle request for root
        if path == &PathBuf::from("./root") {
            return Ok(root.clone());
        }

        // Handle relative paths from root
        if let Some(rel_path) = path.strip_prefix("./root/").ok() {
            let mut current = root;
            for component in rel_path.components() {
                if let Some(name) = component.as_os_str().to_str() {
                    current = current
                        .children
                        .get(name)
                        .ok_or_else(|| anyhow::anyhow!("Path not found: {}", path.display()))?;
                }
            }
            return Ok(current.clone());
        }

        // Handle paths without ./root prefix
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid path: {}", path.display()))?;
        let entry = root
            .children
            .get(path_str)
            .ok_or_else(|| anyhow::anyhow!("Path not found: {}", path.display()))?;

        Ok(entry.clone())
    }

    fn apply_replacements(&self, path: &Path, replacements: &[FileReplacement]) -> Result<String> {
        let mut files = self.files.lock().unwrap();

        let content = files
            .get(path)
            .ok_or_else(|| anyhow::anyhow!("File not found: {}", path.display()))?
            .clone();

        let updated_content = crate::utils::apply_replacements_normalized(&content, replacements)?;

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
                    let context_lines = 2;
                    let start_line = line_idx.saturating_sub(context_lines);
                    let section_end = (line_idx + context_lines + 1).min(content.lines().count());

                    let mut section_lines = Vec::new();
                    for i in start_line..section_end {
                        section_lines.push(content.lines().nth(i).unwrap().to_string());
                    }

                    results.push(SearchResult {
                        file: file_path.clone(),
                        start_line,
                        line_content: section_lines,
                        match_lines: vec![line_idx - start_line],
                        match_ranges: vec![matches.iter().map(|m| (m.start(), m.end())).collect()],
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
        Tool::ListProjects { .. } => "list_projects",
        Tool::UpdatePlan { .. } => "update_plan",
        Tool::SearchFiles { .. } => "search_files",
        Tool::ExecuteCommand { .. } => "execute_command",
        Tool::ListFiles { .. } => "list_files",
        Tool::ReadFiles { .. } => "read_files",
        Tool::WriteFile { .. } => "write_file",
        Tool::ReplaceInFile { .. } => "replace_in_file",
        Tool::DeleteFiles { .. } => "delete_files",
        Tool::Summarize { .. } => "summarize",
        Tool::CompleteTask { .. } => "complete_task",
        Tool::UserInput { .. } => "user_input",
        Tool::WebSearch { .. } => "web_search",
        Tool::WebFetch { .. } => "web_fetch",
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
        Tool::Summarize {
            project,
            path,
            summary,
        } => serde_json::json!({
            "project": project,
            "path": path,
            "summary": summary
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

fn create_explorer_mock() -> MockExplorer {
    let mut files = HashMap::new();
    files.insert(
        PathBuf::from("./root/test.txt"),
        "line 1\nline 2\nline 3\n".to_string(),
    );

    // Add src directory to tree
    let mut root_children = HashMap::new();
    root_children.insert(
        "src".to_string(),
        FileTreeEntry {
            name: "src".to_string(),
            entry_type: FileSystemEntryType::Directory,
            children: HashMap::new(),
            is_expanded: true,
        },
    );

    let file_tree = Some(FileTreeEntry {
        name: "./root".to_string(),
        entry_type: FileSystemEntryType::Directory,
        children: root_children,
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
        r.line_content.iter().all(|line| {
            // Check that "line" is not part of another word
            !line.contains(&"inline".to_string())
                && !line.contains(&"pipeline".to_string())
                && !line.contains(&"airline".to_string())
        })
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
    assert!(results.iter().any(|r| r
        .line_content
        .iter()
        .any(|line| line.contains(&"line 1".to_string()))));

    // Test regex search
    let results = explorer.search(
        &PathBuf::from("./root"),
        SearchOptions {
            query: r"line \d+".to_string(), // Match "line" followed by numbers
            mode: SearchMode::Regex,
            ..Default::default()
        },
    )?;
    assert!(results.iter().any(|r| r
        .line_content
        .iter()
        .any(|line| line.contains(&"line 1".to_string()))));

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
async fn test_agent_read_files() -> Result<(), anyhow::Error> {
    // Test success case (full file)
    let mock_llm = MockLLMProvider::new(vec![Ok(create_test_response(
        Tool::ReadFiles {
            project: "test".to_string(),
            paths: vec![PathBuf::from("test.txt")],
        },
        "Reading test file (full content)",
    ))]);
    // Obtain a reference to the mock_llm before handing ownership to the agent
    let mock_llm_ref = mock_llm.clone();

    let mut agent = Agent::new(
        Box::new(mock_llm),
        ToolMode::Native,
        AgentMode::WorkingMemory,
        Box::new(MockProjectManager::new()),
        Box::new(create_command_executor_mock()),
        Box::new(MockUI::default()),
        Box::new(MockStatePersistence::new()),
        Some(PathBuf::from("./test_path")),
    );

    // Run the agent
    agent.start_with_task("Test task".to_string()).await?;

    // Verify the file is displayed in the working memory of the second request
    let locked_requests = mock_llm_ref.requests.lock().unwrap();
    let second_request = &locked_requests[1];

    if let MessageContent::Text(content) = &second_request.messages[0].content {
        assert!(
            content.contains(
                ">>>>> RESOURCE: [test] test.txt\nline 1\nline 2\nline 3\n\n<<<<< END RESOURCE"
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
async fn test_agent_read_files_with_line_range() -> Result<(), anyhow::Error> {
    // Test with line range (only lines 1-2)
    let mock_llm = MockLLMProvider::new(vec![Ok(create_test_response(
        Tool::ReadFiles {
            project: "test".to_string(),
            paths: vec![PathBuf::from("test.txt:1-2")],
        },
        "Reading test file (limited range)",
    ))]);
    // Obtain a reference to the mock_llm
    let mock_llm_ref = mock_llm.clone();

    let mut agent = Agent::new(
        Box::new(mock_llm),
        ToolMode::Native,
        AgentMode::WorkingMemory,
        Box::new(MockProjectManager::new()),
        Box::new(create_command_executor_mock()),
        Box::new(MockUI::default()),
        Box::new(MockStatePersistence::new()),
        Some(PathBuf::from("./test_path")),
    );

    // Run the agent
    agent
        .start_with_task("Test task with line range".to_string())
        .await?;

    // Verify only the specified lines are displayed
    let locked_requests = mock_llm_ref.requests.lock().unwrap();
    let second_request = &locked_requests[1];

    if let MessageContent::Text(content) = &second_request.messages[0].content {
        assert!(
            content.contains(
                ">>>>> RESOURCE: [test] test.txt:1-2\nline 1\nline 2\n<<<<< END RESOURCE"
            ),
            "File content not found or incorrect in working memory message:\n{}",
            content
        );
        // Verify line 3 is NOT included
        assert!(
            !content.contains("line 3"),
            "Line 3 should not be present in the output:\n{}",
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
        output: "command output".to_string(),
    };

    let mock_command_executor = MockCommandExecutor::new(vec![Ok(test_output)]);
    let mock_command_executor_ref = mock_command_executor.clone();

    let mock_llm = MockLLMProvider::new(vec![Ok(create_test_response(
        Tool::ExecuteCommand {
            project: "test".to_string(),
            command_line: "test command".to_string(),
            working_dir: None,
        },
        "Testing command execution",
    ))]);

    let mut agent = Agent::new(
        Box::new(mock_llm),
        ToolMode::Native,
        AgentMode::WorkingMemory,
        Box::new(MockProjectManager::new()),
        Box::new(mock_command_executor),
        Box::new(MockUI::default()),
        Box::new(MockStatePersistence::new()),
        Some(PathBuf::from("./test_path")),
    );

    // Run the agent
    agent.start_with_task("Test task".to_string()).await?;

    // Verify number of calls and command parameters
    assert_eq!(mock_command_executor_ref.calls.load(Ordering::Relaxed), 1);

    let captured_commands = mock_command_executor_ref.get_captured_commands();
    assert_eq!(captured_commands.len(), 1);
    assert_eq!(captured_commands[0].0, "test command");
    assert_eq!(
        captured_commands[0].1.as_ref().map(|p| p.to_str().unwrap()),
        Some("./root")
    );

    Ok(())
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
        usage: Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    };

    // Use a test request_id
    let request_id = 42;

    let actions = parse_llm_response(&response, request_id)?;
    assert_eq!(actions.len(), 1);
    assert!(actions[0].reasoning.contains("search for TODO comments"));

    if let Tool::SearchFiles { regex, .. } = &actions[0].tool {
        assert_eq!(regex, "TODO & FIXME <html>"); // Notice the & character is allowed and also tags
    } else {
        panic!("Expected Search tool");
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
    let actions = parse_llm_response(&response, request_id)?;
    assert_eq!(actions.len(), 1);
    assert!(actions[0].reasoning.contains("fix the code formatting"));

    if let Tool::ReplaceInFile {
        project,
        path,
        replacements,
    } = &actions[0].tool
    {
        assert_eq!(project, "test");
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
            replace_all: false,
        },
        FileReplacement {
            search: "Goodbye".to_string(),
            replace: "See you".to_string(),
            replace_all: false,
        },
    ];

    let result = explorer.apply_replacements(&PathBuf::from("./root/test.txt"), &replacements)?;

    assert_eq!(result, "Hi there\nThis is a test\nSee you");
    Ok(())
}

#[tokio::test]
async fn test_replace_in_file_error_handling() -> Result<()> {
    // Setup a scenario where a file replacement fails first (wrong search string),
    // then succeeds with corrected search string
    let initial_content = "function test() {\n    console.log(\"test\");\n}\n";

    // First a read action to get the file into working memory
    let mock_llm = MockLLMProvider::new(vec![
        Ok(create_test_response(
            Tool::ReplaceInFile {
                project: "test".to_string(),
                path: PathBuf::from("test.rs"),
                replacements: vec![FileReplacement {
                    search: "function test()".to_string(), // correct
                    replace: "fn test()".to_string(),
                    replace_all: false,
                }],
            },
            "Trying with correct search string",
        )),
        Ok(create_test_response(
            Tool::ReplaceInFile {
                project: "test".to_string(),
                path: PathBuf::from("test.rs"),
                replacements: vec![FileReplacement {
                    search: "wrong search".to_string(), // will fail
                    replace: "fn test()".to_string(),
                    replace_all: false,
                }],
            },
            "Initial attempt to replace",
        )),
        Ok(create_test_response(
            Tool::ReadFiles {
                project: "test".to_string(),
                paths: vec![PathBuf::from("test.rs")],
            },
            "Reading test file",
        )),
    ]);
    let mock_llm_ref = mock_llm.clone();

    // File exists and has content
    let mock_explorer = MockExplorer::new(
        HashMap::from([(PathBuf::from("./root/test.rs"), initial_content.to_string())]),
        Some(FileTreeEntry {
            name: "./root".to_string(),
            entry_type: FileSystemEntryType::Directory,
            children: HashMap::new(),
            is_expanded: true,
        }),
    );

    // Create a ProjectManager with our mock explorer
    let project_manager =
        MockProjectManager::new().with_project("test", PathBuf::from("./root"), mock_explorer);

    let mut agent = Agent::new(
        Box::new(mock_llm),
        ToolMode::Native,
        AgentMode::WorkingMemory,
        Box::new(project_manager),
        Box::new(create_command_executor_mock()),
        Box::new(MockUI::default()),
        Box::new(MockStatePersistence::new()),
        Some(PathBuf::from("./test_path")),
    );

    // Run the agent
    agent
        .start_with_task("Convert JavaScript function to Rust".to_string())
        .await?;

    // Check that error was communicated to LLM
    let requests = mock_llm_ref.requests.lock().unwrap();

    // Should see four requests:
    // 1. Initial ReadFiles
    // 2. Failed ReplaceInFile
    // 3. Corrected ReplaceInFile
    // 4. CompleteTask
    assert_eq!(requests.len(), 4);

    // The error message should be a user message in the third request
    let error_request = &requests[2];
    assert_eq!(error_request.messages.len(), 3); // Working Memory + Tool Response + Error
    if let MessageContent::Structured(content_blocks) = &error_request.messages[2].content {
        assert!(
            content_blocks.len() == 1,
            "Expected there to be one content block, got: {}",
            content_blocks.len()
        );

        if let ContentBlock::ToolResult { content, .. } = &content_blocks[0] {
            assert!(
                content.contains("Could not find SEARCH block"),
                "Expected error message about missing search content, got:\n{}",
                content
            );
        } else {
            panic!("Expected ContentBlock::ToolResult but got a different variant");
        }
    } else {
        panic!("Expected error message to be content blocks");
    }

    Ok(())
}

#[tokio::test]
async fn test_list_files_error_handling() -> Result<()> {
    let mock_llm = MockLLMProvider::new(vec![
        Ok(create_test_response(
            Tool::ListFiles {
                project: "test".to_string(),
                paths: vec![PathBuf::from("src")],
                max_depth: None,
            },
            "Listing files with correct path",
        )),
        Ok(create_test_response(
            Tool::ListFiles {
                project: "test".to_string(),
                paths: vec![PathBuf::from("nonexistent")],
                max_depth: None,
            },
            "Initial attempt to list files",
        )),
    ]);
    let mock_llm_ref = mock_llm.clone();

    let mut agent = Agent::new(
        Box::new(mock_llm),
        ToolMode::Native,
        AgentMode::WorkingMemory,
        Box::new(MockProjectManager::new()),
        Box::new(create_command_executor_mock()),
        Box::new(MockUI::default()),
        Box::new(MockStatePersistence::new()),
        Some(PathBuf::from("./test_path")),
    );

    agent
        .start_with_task("List project files".to_string())
        .await?;

    let requests = mock_llm_ref.requests.lock().unwrap();

    // Should see three requests:
    // 1. Failed ListFiles
    // 2. Corrected ListFiles
    // 3. CompleteTask
    assert_eq!(requests.len(), 3);

    // The error message should be a user message in the second request
    let error_request = &requests[1];
    assert_eq!(error_request.messages.len(), 3); // Working Memory + Tool Response + Error
    if let MessageContent::Text(content) = &error_request.messages[2].content {
        println!("{}", content);
        assert!(content.contains("Error executing action"));
        assert!(content.contains("Path not found"));
    }

    Ok(())
}

#[tokio::test]
async fn test_read_files_error_handling() -> Result<()> {
    let mock_llm = MockLLMProvider::new(vec![
        Ok(create_test_response(
            Tool::ReadFiles {
                project: "test".to_string(),
                paths: vec![PathBuf::from("test.txt")],
            },
            "Reading existing file",
        )),
        Ok(create_test_response(
            Tool::ReadFiles {
                project: "test".to_string(),
                paths: vec![PathBuf::from("nonexistent.txt")],
            },
            "Attempting to read non-existent file",
        )),
    ]);
    let mock_llm_ref = mock_llm.clone();

    let mut agent = Agent::new(
        Box::new(mock_llm),
        ToolMode::Native,
        AgentMode::WorkingMemory,
        Box::new(MockProjectManager::new()),
        Box::new(create_command_executor_mock()),
        Box::new(MockUI::default()),
        Box::new(MockStatePersistence::new()),
        Some(PathBuf::from("./test_path")),
    );

    agent
        .start_with_task("Read file contents".to_string())
        .await?;

    let requests = mock_llm_ref.requests.lock().unwrap();

    // Should see three requests:
    // 1. Failed ReadFiles
    // 2. Corrected ReadFiles
    // 3. CompleteTask
    assert_eq!(requests.len(), 3);

    // The error message should be a user message in the second request
    let error_request = &requests[1];
    assert_eq!(error_request.messages.len(), 3); // Working Memory + Tool Response + Error
    if let MessageContent::Text(content) = &error_request.messages[2].content {
        assert!(content.contains("Error executing action"));
        assert!(content.contains("File not found"));
    }

    Ok(())
}

#[tokio::test]
async fn test_write_file_error_handling() -> Result<()> {
    let mock_llm = MockLLMProvider::new(vec![
        Ok(create_test_response(
            Tool::WriteFile {
                project: "test".to_string(),
                path: PathBuf::from("test.txt"),
                content: "valid content".to_string(),
                append: false,
            },
            "Writing to valid path",
        )),
        Ok(create_test_response(
            Tool::WriteFile {
                project: "test".to_string(),
                path: PathBuf::from("/invalid/path/test.txt"),
                content: "test content".to_string(),
                append: false,
            },
            "Attempting to write to invalid absolute path",
        )),
    ]);
    let mock_llm_ref = mock_llm.clone();

    let mut agent = Agent::new(
        Box::new(mock_llm),
        ToolMode::Native,
        AgentMode::WorkingMemory,
        Box::new(MockProjectManager::new()),
        Box::new(create_command_executor_mock()),
        Box::new(MockUI::default()),
        Box::new(MockStatePersistence::new()),
        Some(PathBuf::from("./test_path")),
    );

    agent
        .start_with_task("Write file contents".to_string())
        .await?;

    mock_llm_ref.print_requests();
    let requests = mock_llm_ref.requests.lock().unwrap();

    // Should see three requests:
    // 1. Failed WriteFile
    // 2. Corrected WriteFile
    // 3. CompleteTask
    assert_eq!(requests.len(), 3);

    // The error message should be a user message in the second request
    let error_request = &requests[1];
    assert_eq!(error_request.messages.len(), 3); // Working Memory + Tool Response + Error
    if let MessageContent::Text(content) = &error_request.messages[2].content {
        assert!(content.contains("Error executing action"));
        assert!(content.contains("absolute path"));
    }

    Ok(())
}

#[tokio::test]
async fn test_read_files_line_range_error_handling() -> Result<()> {
    let mock_llm = MockLLMProvider::new(vec![
        Ok(create_test_response(
            Tool::ReadFiles {
                project: "test".to_string(),
                paths: vec![PathBuf::from("test.txt")],
            },
            "Reading existing file with valid line range",
        )),
        Ok(create_test_response(
            Tool::ReadFiles {
                project: "test".to_string(),
                paths: vec![PathBuf::from("test.txt:10-20")],
            },
            "Attempting to read with invalid line range",
        )),
    ]);
    let mock_llm_ref = mock_llm.clone();

    let mut agent = Agent::new(
        Box::new(mock_llm),
        ToolMode::Native,
        AgentMode::WorkingMemory,
        Box::new(MockProjectManager::new()),
        Box::new(create_command_executor_mock()),
        Box::new(MockUI::default()),
        Box::new(MockStatePersistence::new()),
        Some(PathBuf::from("./test_path")),
    );

    agent
        .start_with_task("Read file with line range".to_string())
        .await?;

    let requests = mock_llm_ref.requests.lock().unwrap();

    // Should see three requests:
    // 1. Failed ReadFiles with invalid line range
    // 2. Corrected ReadFiles with valid line range
    // 3. CompleteTask
    assert_eq!(requests.len(), 3);

    // The error message should be a user message in the second request
    let error_request = &requests[1];
    assert_eq!(error_request.messages.len(), 3); // Working Memory + Tool Response + Error
    if let MessageContent::Structured(content_blocks) = &error_request.messages[2].content {
        assert!(
            content_blocks.len() == 1,
            "Expected there to be one content block, got: {}",
            content_blocks.len()
        );

        if let ContentBlock::ToolResult {
            content, is_error, ..
        } = &content_blocks[0]
        {
            assert!(content.contains("Invalid line range")); // Check for specific line range error
            if let Some(is_error) = is_error {
                assert!(is_error)
            } else {
                panic!("Expected is_error to be present");
            }
        } else {
            panic!("Expected ContentBlock::ToolResult but got a different variant");
        }
    } else {
        panic!("Expected error message to be content blocks");
    }

    Ok(())
}

#[tokio::test]
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
        AgentMode::WorkingMemory,
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
    assert_eq!(error_request.messages.len(), 3); // Working Memory + Tool Response + Error
    if let MessageContent::Text(content) = &error_request.messages[2].content {
        assert!(content.contains("Unknown tool"));
        assert!(content.contains("Please use only available tools"));
    } else {
        panic!("Expected error message to be text content");
    }

    Ok(())
}

#[tokio::test]
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
        AgentMode::WorkingMemory,
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
    assert_eq!(error_request.messages.len(), 3); // Working Memory + Tool Response + Error
    if let MessageContent::Text(content) = &error_request.messages[2].content {
        assert!(content.contains("Tool parameter error"));
        assert!(content.contains("Please try again"));
    } else {
        panic!("Expected error message to be text content");
    }

    Ok(())
}
