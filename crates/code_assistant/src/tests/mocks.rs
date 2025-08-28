use crate::config::ProjectManager;
use crate::types::*;
use crate::ui::{UIError, UiEvent, UserInterface};
use crate::utils::{CommandExecutor, CommandOutput};
use anyhow::Result;
use async_trait::async_trait;
use llm::{types::*, LLMProvider, LLMRequest, StreamingCallback};
use regex::RegexBuilder;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

// New MockLLMProvider that works with the trait-based tool system
#[derive(Default, Clone)]
pub struct MockLLMProvider {
    requests: Arc<Mutex<Vec<LLMRequest>>>,
    responses: Arc<Mutex<Vec<Result<LLMResponse, anyhow::Error>>>>,
}

impl MockLLMProvider {
    pub fn new(mut responses: Vec<Result<LLMResponse, anyhow::Error>>) -> Self {
        // Add CompleteTask response at the beginning if the first response is ok
        if responses.first().is_some_and(|r| r.is_ok()) {
            responses.insert(
                0,
                Ok(create_test_response(
                    "complete-task-id",
                    "complete_task",
                    serde_json::json!({
                        "message": "Task completed successfully"
                    }),
                    "Completing task after successful execution",
                )),
            );
        }

        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(responses)),
        }
    }

    // Get access to the stored requests
    pub fn get_requests(&self) -> Vec<LLMRequest> {
        self.requests.lock().unwrap().clone()
    }

    #[allow(dead_code)]
    pub fn print_requests(&self) {
        let requests = self.requests.lock().unwrap();
        println!("\nTotal number of requests: {}", requests.len());
        for (i, request) in requests.iter().enumerate() {
            println!("\nRequest {i}:");
            for (j, message) in request.messages.iter().enumerate() {
                println!("  Message {j}:");
                // Using the Display trait implementation for Message
                let formatted_message = format!("{message}");
                // Add indentation to the message output
                let indented = formatted_message
                    .lines()
                    .map(|line| format!("    {line}"))
                    .collect::<Vec<String>>()
                    .join("\n");
                println!("{indented}");
            }
        }
    }
}

#[async_trait]
impl LLMProvider for MockLLMProvider {
    async fn send_message(
        &mut self,
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

// Helper function to create a test response for tool invocation
pub fn create_test_response(
    tool_id: &str,
    tool_name: &str,
    tool_input: serde_json::Value,
    reasoning: &str,
) -> LLMResponse {
    LLMResponse {
        content: vec![
            ContentBlock::Text {
                text: reasoning.to_string(),
            },
            ContentBlock::ToolUse {
                id: tool_id.to_string(),
                name: tool_name.to_string(),
                input: tool_input,
            },
        ],
        usage: Usage::zero(),
        rate_limit_info: None,
    }
}

pub fn create_test_response_text(text: &str) -> LLMResponse {
    LLMResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        usage: Usage::zero(),
        rate_limit_info: None,
    }
}

// Struct to represent a captured command
#[derive(Clone, Debug)]
pub struct CapturedCommand {
    pub command_line: String,
    pub working_dir: Option<PathBuf>,
}

// Mock CommandExecutor
#[derive(Clone)]
pub struct MockCommandExecutor {
    responses: Arc<Mutex<Vec<Result<CommandOutput, anyhow::Error>>>>,
    calls: Arc<AtomicUsize>,
    captured_commands: Arc<Mutex<Vec<CapturedCommand>>>,
}

impl MockCommandExecutor {
    pub fn new(responses: Vec<Result<CommandOutput, anyhow::Error>>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            calls: Arc::new(AtomicUsize::new(0)),
            captured_commands: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn get_captured_commands(&self) -> Vec<CapturedCommand> {
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
            .push(CapturedCommand {
                command_line: command_line.to_string(),
                working_dir: working_dir.cloned(),
            });

        self.responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Err(anyhow::anyhow!("No more mock responses")))
    }
}

// Create a mock with successful execution
pub fn create_command_executor_mock() -> MockCommandExecutor {
    MockCommandExecutor::new(vec![Ok(CommandOutput {
        success: true,
        output: "Command output".to_string(),
    })])
}

// Create a mock with failed execution
pub fn create_failed_command_executor_mock() -> MockCommandExecutor {
    MockCommandExecutor::new(vec![Ok(CommandOutput {
        success: false,
        output: "Command failed: permission denied".to_string(),
    })])
}

// Mock UI
#[derive(Default, Clone)]
pub struct MockUI {
    events: Arc<Mutex<Vec<UiEvent>>>,
    streaming: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl UserInterface for MockUI {
    async fn send_event(&self, event: UiEvent) -> Result<(), UIError> {
        self.events.lock().unwrap().push(event);
        Ok(())
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
            crate::ui::DisplayFragment::Image { media_type, .. } => {
                self.streaming
                    .lock()
                    .unwrap()
                    .push(format!("\n• {media_type}"));
            }
            crate::ui::DisplayFragment::ToolName { name, .. } => {
                self.streaming
                    .lock()
                    .unwrap()
                    .push(format!("\n• Image {name}"));
            }
            crate::ui::DisplayFragment::ToolParameter { name, value, .. } => {
                self.streaming
                    .lock()
                    .unwrap()
                    .push(format!("  {name}: {value}"));
            }
            crate::ui::DisplayFragment::ToolEnd { .. } => {}
        }
        Ok(())
    }

    fn should_streaming_continue(&self) -> bool {
        // Mock implementation always continues streaming
        true
    }

    fn notify_rate_limit(&self, _seconds_remaining: u64) {
        // Mock implementation does nothing with rate limit notifications
    }

    fn clear_rate_limit(&self) {
        // Mock implementation does nothing with rate limit clearing
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// Mock Explorer
#[derive(Default, Clone)]
pub struct MockExplorer {
    files: Arc<Mutex<HashMap<PathBuf, String>>>,
    file_tree: Arc<Mutex<Option<FileTreeEntry>>>,
    // Optional map of formatted results to apply after a formatting command runs
    formatted_after: Arc<Mutex<HashMap<PathBuf, String>>>,
}

impl MockExplorer {
    pub fn new(files: HashMap<PathBuf, String>, file_tree: Option<FileTreeEntry>) -> Self {
        Self {
            files: Arc::new(Mutex::new(files)),
            file_tree: Arc::new(Mutex::new(file_tree)),
            formatted_after: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Create a MockExplorer that simulates formatting by applying provided formatted content
    /// after a formatting command is executed. The initial file contents are used for edits,
    /// then when a formatting command is run, the content for that path is replaced with
    /// the provided formatted content (if present in the map).
    pub fn new_with_formatting(
        initial_files: HashMap<PathBuf, String>,
        formatted_files: HashMap<PathBuf, String>,
        file_tree: Option<FileTreeEntry>,
    ) -> Self {
        Self {
            files: Arc::new(Mutex::new(initial_files)),
            file_tree: Arc::new(Mutex::new(file_tree)),
            formatted_after: Arc::new(Mutex::new(formatted_files)),
        }
    }

    #[allow(dead_code)]
    pub fn print_files(&self) {
        let files = self.files.lock().unwrap();
        println!("\nMock files contents:");
        for (path, contents) in files.iter() {
            println!("- {}:", path.display());
            println!("{contents}");
        }
    }
}

#[async_trait::async_trait]
impl CodeExplorer for MockExplorer {
    fn clone_box(&self) -> Box<dyn CodeExplorer> {
        Box::new(MockExplorer {
            files: self.files.clone(),
            file_tree: self.file_tree.clone(),
            formatted_after: self.formatted_after.clone(),
        })
    }

    fn root_dir(&self) -> PathBuf {
        PathBuf::from("./root")
    }

    fn read_file(&self, path: &Path) -> Result<String, anyhow::Error> {
        self.files
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("File not found: {}", path.display()))
    }

    fn read_file_range(
        &self,
        path: &Path,
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

    fn write_file(&self, path: &Path, content: &str, append: bool) -> Result<String> {
        // Check parent directories
        for component in path.parent().unwrap_or(path).components() {
            let current = PathBuf::from(component.as_os_str());
            if self.files.lock().unwrap().get(&current).is_some() {
                // If any parent is a file (has content), that's an error
                return Err(anyhow::anyhow!(
                    "Cannot create file: {} is a file",
                    current.display()
                ));
            }
        }

        let mut files = self.files.lock().unwrap();
        let result_content;

        if append && files.contains_key(path) {
            // Append content to existing file
            if let Some(existing) = files.get_mut(path) {
                *existing = format!("{existing}{content}");
                result_content = existing.clone();
            } else {
                result_content = content.to_string();
            }
        } else {
            // Write or overwrite file
            files.insert(path.to_path_buf(), content.to_string());
            result_content = content.to_string();
        }

        Ok(result_content)
    }

    fn delete_file(&self, path: &Path) -> Result<()> {
        let mut files = self.files.lock().unwrap();
        if files.contains_key(path) {
            files.remove(path);
            Ok(())
        } else {
            Err(anyhow::anyhow!("File not found: {}", path.display()))
        }
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
        path: &Path,
        _max_depth: Option<usize>,
    ) -> Result<FileTreeEntry, anyhow::Error> {
        let file_tree = self.file_tree.lock().unwrap();
        let root = file_tree
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No file tree configured"))?;

        // Handle request for root
        if path == PathBuf::from("./root") {
            return Ok(root.clone());
        }

        // Handle relative paths from root
        if let Ok(rel_path) = path.strip_prefix("./root/") {
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

    async fn apply_replacements_with_formatting(
        &self,
        path: &Path,
        replacements: &[FileReplacement],
        format_command: &str,
        command_executor: &dyn crate::utils::CommandExecutor,
    ) -> Result<(String, Option<Vec<FileReplacement>>)> {
        use crate::utils::file_updater::{
            extract_stable_ranges, find_replacement_matches, reconstruct_formatted_replacements,
        };

        // Capture original content
        let original_content = self.read_file(path)?;

        // Find matches and detect adjacency/overlap
        let (matches, has_conflicts) = find_replacement_matches(&original_content, replacements)?;

        // Apply replacements first
        let updated_content = self.apply_replacements(path, replacements)?;

        // Execute the format command to simulate formatting
        let output = command_executor
            .execute(format_command, Some(&PathBuf::from("./root")))
            .await?;

        // If formatting failed, do not attempt to reconstruct replacements
        if !output.success {
            return Ok((updated_content, None));
        }

        // After formatting command, if we have a formatted version for this path, apply it
        let final_content = if let Some(formatted) = self
            .formatted_after
            .lock()
            .unwrap()
            .get(path)
            .cloned()
        {
            // Replace file contents with the formatted version
            self.files
                .lock()
                .unwrap()
                .insert(path.to_path_buf(), formatted.clone());
            formatted
        } else {
            updated_content.clone()
        };

        // Try to reconstruct updated replacements if there are no conflicts
        let updated_replacements = if has_conflicts {
            None
        } else {
            let stable_ranges = extract_stable_ranges(&original_content, &matches);
            reconstruct_formatted_replacements(
                &original_content,
                &final_content,
                &stable_ranges,
                &matches,
                replacements,
            )
        };

        Ok((final_content, updated_replacements))
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
    assert!(!results.is_empty()); // Should find matches

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

#[test]
fn test_mock_explorer_apply_replacements() -> Result<(), anyhow::Error> {
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

pub fn create_explorer_mock() -> MockExplorer {
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

#[derive(Default)]
pub struct MockProjectManager {
    explorers: HashMap<String, Box<dyn CodeExplorer>>,
    projects: HashMap<String, Project>,
}

impl MockProjectManager {
    pub fn new() -> Self {
        let empty = Self {
            explorers: HashMap::new(),
            projects: HashMap::new(),
        };
        // Add default project
        empty.with_project_path(
            "test",
            PathBuf::from("./root"),
            Box::new(create_explorer_mock()),
        )
    }

    // Helper to add a custom project and explorer
    pub fn with_project_path(
        self,
        name: &str,
        path: PathBuf,
        explorer: Box<dyn CodeExplorer>,
    ) -> Self {
        self.with_project(
            name,
            Project {
                path,
                format_on_save: None,
            },
            explorer,
        )
    }

    // Helper to add a custom project and explorer
    pub fn with_project(
        mut self,
        name: &str,
        project: Project,
        explorer: Box<dyn CodeExplorer>,
    ) -> Self {
        self.projects.insert(name.to_string(), project);
        self.explorers.insert(name.to_string(), explorer);
        self
    }
}

impl ProjectManager for MockProjectManager {
    fn add_temporary_project(&mut self, path: PathBuf) -> Result<String> {
        // Use a fixed name for testing
        let project_name = "temp_project".to_string();

        // Add the project
        self.projects.insert(
            project_name.clone(),
            Project {
                path: path.clone(),
                format_on_save: None,
            },
        );

        // Add a default explorer for it
        self.explorers
            .insert(project_name.clone(), Box::new(create_explorer_mock()));

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
            Some(explorer) => Ok(explorer.clone_box()),
            None => Err(anyhow::anyhow!("Project {} not found", name)),
        }
    }
}
