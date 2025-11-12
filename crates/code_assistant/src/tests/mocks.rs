use crate::config::ProjectManager;
use crate::tools::core::tool::ToolContext;
use crate::types::*;
use crate::ui::{UIError, UiEvent, UserInterface};
use anyhow::Result;
use async_trait::async_trait;
use command_executor::{CommandExecutor, CommandOutput, SandboxCommandRequest, StreamingCallback};
use fs_explorer::{
    file_updater::{
        apply_replacements_normalized, extract_stable_ranges, find_replacement_matches,
        reconstruct_formatted_replacements,
    },
    CodeExplorer, FileReplacement, FileSystemEntryType, FileTreeEntry, SearchMode, SearchOptions,
    SearchResult,
};
use llm::{types::*, LLMProvider, LLMRequest, StreamingCallback as LlmStreamingCallback};
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
        _streaming_callback: Option<&LlmStreamingCallback>,
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
            ContentBlock::new_text(reasoning),
            ContentBlock::new_tool_use(tool_id, tool_name, tool_input),
        ],
        usage: Usage::zero(),
        rate_limit_info: None,
    }
}

pub fn create_test_response_text(text: &str) -> LLMResponse {
    LLMResponse {
        content: vec![ContentBlock::new_text(text)],
        usage: Usage::zero(),
        rate_limit_info: None,
    }
}

// Struct to represent a captured command
#[derive(Clone, Debug)]
pub struct CapturedCommand {
    pub command_line: String,
    pub working_dir: Option<PathBuf>,
    #[allow(dead_code)]
    pub sandbox_request: Option<SandboxCommandRequest>,
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
        sandbox_request: Option<&SandboxCommandRequest>,
    ) -> Result<CommandOutput> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.captured_commands
            .lock()
            .unwrap()
            .push(CapturedCommand {
                command_line: command_line.to_string(),
                working_dir: working_dir.cloned(),
                sandbox_request: sandbox_request.cloned(),
            });

        self.responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Err(anyhow::anyhow!("No more mock responses")))
    }

    async fn execute_streaming(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
        callback: Option<&dyn StreamingCallback>,
        sandbox_request: Option<&SandboxCommandRequest>,
    ) -> Result<CommandOutput> {
        // For mock, just call the regular execute and simulate streaming if callback provided
        let result = self
            .execute(command_line, working_dir, sandbox_request)
            .await?;

        if let Some(callback) = callback {
            // Simulate streaming by sending the output in chunks
            for line in result.output.lines() {
                let _ = callback.on_output_chunk(&format!("{line}\n"));
            }
        }

        Ok(result)
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
#[allow(dead_code)]
pub fn create_failed_command_executor_mock() -> MockCommandExecutor {
    MockCommandExecutor::new(vec![Ok(CommandOutput {
        success: false,
        output: "Command failed: permission denied".to_string(),
    })])
}

// Helper to create a test ToolContext with all required fields
#[allow(dead_code)]
pub fn create_test_tool_context<'a>(
    project_manager: &'a dyn crate::config::ProjectManager,
    command_executor: &'a dyn CommandExecutor,
    working_memory: Option<&'a mut crate::types::WorkingMemory>,
    plan: Option<&'a mut crate::types::PlanState>,
    ui: Option<&'a dyn crate::ui::UserInterface>,
    tool_id: Option<String>,
) -> crate::tools::core::ToolContext<'a> {
    crate::tools::core::ToolContext {
        project_manager,
        command_executor,
        working_memory,
        plan,
        ui,
        tool_id,
    }
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
            crate::ui::DisplayFragment::ToolOutput { chunk, .. } => {
                self.streaming.lock().unwrap().push(chunk.clone());
            }
            crate::ui::DisplayFragment::ToolTerminal { terminal_id, .. } => {
                self.streaming
                    .lock()
                    .unwrap()
                    .push(format!("[terminal:{terminal_id}]"));
            }
            crate::ui::DisplayFragment::ReasoningSummaryStart => {
                // Ignore start markers in mock output
            }
            crate::ui::DisplayFragment::ReasoningSummaryDelta(delta) => {
                self.streaming.lock().unwrap().push(delta.clone());
            }
            crate::ui::DisplayFragment::ReasoningComplete => {
                self.streaming
                    .lock()
                    .unwrap()
                    .push("\n• Reasoning Complete".to_string());
            }
            crate::ui::DisplayFragment::CompactionDivider { summary } => {
                self.streaming
                    .lock()
                    .unwrap()
                    .push(format!("[compaction] {summary}"));
            }
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

impl MockUI {
    pub fn events(&self) -> Vec<UiEvent> {
        self.events.lock().unwrap().clone()
    }

    pub fn get_streaming_output(&self) -> Vec<String> {
        self.streaming.lock().unwrap().clone()
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

    async fn read_file(&self, path: &Path) -> Result<String, anyhow::Error> {
        self.files
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("File not found: {}", path.display()))
    }

    async fn read_file_range(
        &self,
        path: &Path,
        start_line: Option<usize>,
        end_line: Option<usize>,
    ) -> Result<String, anyhow::Error> {
        let content = self.read_file(path).await?;

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

    async fn write_file(&self, path: &Path, content: &str, append: bool) -> Result<String> {
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

    async fn delete_file(&self, path: &Path) -> Result<()> {
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

    async fn list_files(
        &mut self,
        path: &Path,
        _max_depth: Option<usize>,
    ) -> Result<FileTreeEntry, anyhow::Error> {
        let file_tree = self.file_tree.lock().unwrap();
        let root = file_tree
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No file tree configured"))?;

        // Handle request for root
        if path == Path::new("./root") {
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

    async fn apply_replacements(
        &self,
        path: &Path,
        replacements: &[FileReplacement],
    ) -> Result<String> {
        let mut files = self.files.lock().unwrap();

        let content = files
            .get(path)
            .ok_or_else(|| anyhow::anyhow!("File not found: {}", path.display()))?
            .clone();

        let updated_content = apply_replacements_normalized(&content, replacements)?;

        // Update the stored content
        files.insert(path.to_path_buf(), updated_content.clone());

        Ok(updated_content)
    }

    async fn apply_replacements_with_formatting(
        &self,
        path: &Path,
        replacements: &[FileReplacement],
        format_command: &str,
        command_executor: &dyn CommandExecutor,
    ) -> Result<(String, Option<Vec<FileReplacement>>)> {
        // Capture original content
        let original_content = self.read_file(path).await?;

        // Find matches and detect adjacency/overlap
        let (matches, has_conflicts) = find_replacement_matches(&original_content, replacements)?;

        // Apply replacements first
        let updated_content = self.apply_replacements(path, replacements).await?;

        // Execute the format command to simulate formatting
        let output = command_executor
            .execute(format_command, Some(&PathBuf::from("./root")), None)
            .await?;

        // If formatting failed, do not attempt to reconstruct replacements
        if !output.success {
            return Ok((updated_content, None));
        }

        // After formatting command, if we have a formatted version for this path, apply it
        let final_content =
            if let Some(formatted) = self.formatted_after.lock().unwrap().get(path).cloned() {
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

    async fn search(
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

#[tokio::test]
async fn test_mock_explorer_search() -> Result<(), anyhow::Error> {
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
    let results = explorer
        .search(
            &PathBuf::from("./root"),
            SearchOptions {
                query: "matching".to_string(),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(results.len(), 2);
    assert!(results.iter().any(|r| r.file.ends_with("test2.txt")));
    assert!(results.iter().any(|r| r.file.ends_with("test3.txt")));

    // Test case-sensitive search
    let results = explorer
        .search(
            &PathBuf::from("./root"),
            SearchOptions {
                query: "LINE".to_string(),
                case_sensitive: true,
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(results.len(), 0); // Should find nothing with case-sensitive search

    // Test case-insensitive search
    let results = explorer
        .search(
            &PathBuf::from("./root"),
            SearchOptions {
                query: "LINE".to_string(),
                case_sensitive: false,
                ..Default::default()
            },
        )
        .await?;
    assert!(!results.is_empty()); // Should find matches

    // Test whole word search
    let results = explorer
        .search(
            &PathBuf::from("./root"),
            SearchOptions {
                query: "line".to_string(),
                whole_words: true,
                ..Default::default()
            },
        )
        .await?;
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
    let results = explorer
        .search(
            &PathBuf::from("./root"),
            SearchOptions {
                query: r"line \d".to_string(),
                mode: SearchMode::Regex,
                ..Default::default()
            },
        )
        .await?;
    assert!(results.iter().any(|r| r
        .line_content
        .iter()
        .any(|line| line.contains(&"line 1".to_string()))));

    // Test regex search
    let results = explorer
        .search(
            &PathBuf::from("./root"),
            SearchOptions {
                query: r"line \d+".to_string(), // Match "line" followed by numbers
                mode: SearchMode::Regex,
                ..Default::default()
            },
        )
        .await?;
    assert!(results.iter().any(|r| r
        .line_content
        .iter()
        .any(|line| line.contains(&"line 1".to_string()))));

    // Test with max_results
    let results = explorer
        .search(
            &PathBuf::from("./root"),
            SearchOptions {
                query: "line".to_string(),
                max_results: Some(2),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(results.len(), 2);

    // Test search in subdirectory
    let results = explorer
        .search(
            &PathBuf::from("./root/subdir"),
            SearchOptions {
                query: "subdir".to_string(),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(results.len(), 1);
    assert!(results[0].file.ends_with("test3.txt"));

    // Test search with no matches
    let results = explorer
        .search(
            &PathBuf::from("./root"),
            SearchOptions {
                query: "nonexistent".to_string(),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(results.len(), 0);

    Ok(())
}

#[tokio::test]
async fn test_mock_explorer_apply_replacements() -> Result<(), anyhow::Error> {
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

    let result = explorer
        .apply_replacements(&PathBuf::from("./root/test.txt"), &replacements)
        .await?;

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
            None => Err(anyhow::anyhow!("Project {name} not found")),
        }
    }
}

/// Test fixture that provides a convenient way to create ToolContext instances for tests
/// while maintaining access to the underlying mocks for assertions
pub struct ToolTestFixture {
    project_manager: MockProjectManager,
    command_executor: MockCommandExecutor,
    working_memory: Option<WorkingMemory>,
    plan: Option<PlanState>,
    ui: Option<MockUI>,
    tool_id: Option<String>,
}

impl ToolTestFixture {
    /// Create a new test fixture with default mocks
    pub fn new() -> Self {
        Self {
            project_manager: MockProjectManager::new(),
            command_executor: MockCommandExecutor::new(vec![]),
            working_memory: None,
            plan: None,
            ui: None,
            tool_id: None,
        }
    }

    /// Create a test fixture with specific files in the default project
    pub fn with_files(files: Vec<(String, String)>) -> Self {
        let mut file_map = HashMap::new();
        let mut children = HashMap::new();

        for (path, content) in files {
            let full_path = PathBuf::from(format!("./root/{path}"));
            file_map.insert(full_path, content);

            // Add to file tree
            if path.contains('/') {
                // Handle nested files in directories
                let parts: Vec<&str> = path.split('/').collect();
                if parts.len() > 1 {
                    let dir_name = parts[0];
                    if !children.contains_key(dir_name) {
                        children.insert(
                            dir_name.to_string(),
                            FileTreeEntry {
                                name: dir_name.to_string(),
                                entry_type: FileSystemEntryType::Directory,
                                children: HashMap::new(),
                                is_expanded: true,
                            },
                        );
                    }
                }
            } else {
                // Handle files in root directory
                children.insert(
                    path.clone(),
                    FileTreeEntry {
                        name: path,
                        entry_type: FileSystemEntryType::File,
                        children: HashMap::new(),
                        is_expanded: false,
                    },
                );
            }
        }

        let file_tree = Some(FileTreeEntry {
            name: "./root".to_string(),
            entry_type: FileSystemEntryType::Directory,
            children,
            is_expanded: true,
        });

        let explorer = MockExplorer::new(file_map, file_tree);
        let project_manager = MockProjectManager::default().with_project_path(
            "test-project",
            PathBuf::from("./root"),
            Box::new(explorer),
        );

        Self {
            project_manager,
            command_executor: MockCommandExecutor::new(vec![]),
            working_memory: None,
            plan: None,
            ui: None,
            tool_id: None,
        }
    }

    /// Create a test fixture with specific command responses
    pub fn with_command_responses(responses: Vec<Result<CommandOutput, anyhow::Error>>) -> Self {
        Self {
            project_manager: MockProjectManager::new(),
            command_executor: MockCommandExecutor::new(responses),
            working_memory: None,
            plan: None,
            ui: None,
            tool_id: None,
        }
    }

    /// Create a test fixture with both files and command responses
    #[allow(dead_code)]
    pub fn with_files_and_commands(
        files: Vec<(String, String)>,
        responses: Vec<Result<CommandOutput, anyhow::Error>>,
    ) -> Self {
        let mut fixture = Self::with_files(files);
        fixture.command_executor = MockCommandExecutor::new(responses);
        fixture
    }

    /// Enable working memory for this fixture
    pub fn with_working_memory(mut self) -> Self {
        self.working_memory = Some(WorkingMemory::default());
        self
    }

    /// Enable plan state for this fixture
    pub fn with_plan(mut self) -> Self {
        self.plan = Some(PlanState::default());
        self
    }

    /// Add a UI mock to this fixture
    pub fn with_ui(mut self) -> Self {
        self.ui = Some(MockUI::default());
        self
    }

    /// Set a tool ID for this fixture
    pub fn with_tool_id(mut self, tool_id: String) -> Self {
        self.tool_id = Some(tool_id);
        self
    }

    /// Create a ToolContext from this fixture
    pub fn context(&mut self) -> ToolContext<'_> {
        ToolContext {
            project_manager: &self.project_manager,
            command_executor: &self.command_executor,
            working_memory: self.working_memory.as_mut(),
            plan: self.plan.as_mut(),
            ui: self.ui.as_ref().map(|ui| ui as &dyn UserInterface),
            tool_id: self.tool_id.clone(),
        }
    }

    /// Get a reference to the command executor for assertions
    pub fn command_executor(&self) -> &MockCommandExecutor {
        &self.command_executor
    }

    /// Get a reference to the project manager for assertions
    #[allow(dead_code)]
    pub fn project_manager(&self) -> &MockProjectManager {
        &self.project_manager
    }

    /// Get a reference to the working memory for assertions
    pub fn working_memory(&self) -> Option<&WorkingMemory> {
        self.working_memory.as_ref()
    }

    /// Get a mutable reference to the working memory for modifications
    pub fn working_memory_mut(&mut self) -> Option<&mut WorkingMemory> {
        self.working_memory.as_mut()
    }

    /// Get a reference to the plan state for assertions
    pub fn plan(&self) -> Option<&PlanState> {
        self.plan.as_ref()
    }

    /// Get a reference to the UI mock for assertions
    pub fn ui(&self) -> Option<&MockUI> {
        self.ui.as_ref()
    }
}
