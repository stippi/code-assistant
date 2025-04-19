use crate::config::ProjectManager;
use crate::types::*;
use crate::ui::{ToolStatus, UIError, UIMessage, UserInterface};
use crate::utils::{CommandExecutor, CommandOutput};
use anyhow::Result;
use async_trait::async_trait;
use regex::RegexBuilder;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

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
pub struct MockExplorer {
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

    fn write_file(&self, path: &PathBuf, content: &String, append: bool) -> Result<String> {
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
        let result_content;

        if append && files.contains_key(path) {
            // Append content to existing file
            if let Some(existing) = files.get_mut(path) {
                *existing = format!("{}{}", existing, content);
                result_content = existing.clone();
            } else {
                result_content = content.clone();
            }
        } else {
            // Write or overwrite file
            files.insert(path.to_path_buf(), content.clone());
            result_content = content.clone();
        }

        Ok(result_content)
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

// Mock ProjectManager for tests
#[derive(Default)]
pub struct MockProjectManager {
    explorers: HashMap<String, MockExplorer>,
    projects: HashMap<String, Project>,
}

impl MockProjectManager {
    pub fn new() -> Self {
        let empty = Self {
            explorers: HashMap::new(),
            projects: HashMap::new(),
        };
        // Add default project
        empty.with_project("test", PathBuf::from("./root"), create_explorer_mock())
    }

    // Helper to add a custom project and explorer
    pub fn with_project(mut self, name: &str, path: PathBuf, explorer: MockExplorer) -> Self {
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
