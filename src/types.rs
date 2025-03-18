use crate::llm::Message;

use crate::web::{WebPage, WebSearchResult};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileTreeEntry {
    pub name: String,
    pub entry_type: FileSystemEntryType,
    pub children: HashMap<String, FileTreeEntry>,
    pub is_expanded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LoadedResource {
    File(String), // File content
    WebSearch {
        query: String,
        results: Vec<WebSearchResult>,
    },
    WebPage(WebPage),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FileEncoding {
    UTF8,
    UTF16LE,
    UTF16BE,
    Windows1252,
    ISO8859_2,
    Other(String),
}

impl Default for FileEncoding {
    fn default() -> Self {
        Self::UTF8
    }
}

/// Represents the agent's working memory during execution
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct WorkingMemory {
    /// Current task description
    pub current_task: String,
    /// Current plan
    pub plan: String,
    /// Memory of previous actions and their results
    pub action_history: Vec<ActionResult>,
    /// Currently loaded resources (files, web search results, web pages)
    pub loaded_resources: HashMap<PathBuf, LoadedResource>,
    /// Summaries of previously seen resources
    pub summaries: HashMap<PathBuf, String>,
    /// Complete file tree of the repository
    pub file_tree: Option<FileTreeEntry>,
}

impl std::fmt::Display for LoadedResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadedResource::File(content) => write!(f, "{}", content),
            LoadedResource::WebSearch { query, results } => {
                writeln!(f, "Web search results for: '{}'", query)?;
                for result in results {
                    writeln!(f, "- {} ({})", result.title, result.url)?;
                    writeln!(f, "  {}", result.snippet)?;
                }
                Ok(())
            }
            LoadedResource::WebPage(page) => {
                writeln!(f, "Content from: {}", page.url)?;
                write!(f, "{}", page.content)
            }
        }
    }
}

impl WorkingMemory {
    /// Convert working memory to markdown format
    pub fn to_markdown(&self) -> String {
        let mut result = String::new();

        // Header
        result.push_str("# Working Memory\n\n");
        result.push_str("This is your accumulated working memory.\n\n");

        // Task
        result.push_str("## Current Task\n\n```\n");
        result.push_str(&self.current_task);
        result.push_str("\n```\n\n");

        // Plan
        result.push_str("## Your Plan\n\n");
        result.push_str(&self.plan);
        result.push_str("\n\n====\n\n");

        // Action history
        result.push_str("## Previous Tools\n\n");
        if self.action_history.is_empty() {
            result.push_str("No actions performed yet\n");
        } else {
            result.push_str("You have already executed the following tools:\n\n");
            // Format action history
            let action_history = self
                .action_history
                .iter()
                .map(|a| {
                    format!(
                        "{:?}\n  Reasoning: {}\n  Result: {}",
                        a.tool,
                        a.reasoning,
                        a.result.format_message()
                    )
                })
                .collect::<Vec<_>>();

            for action in &action_history {
                result.push_str(&format!("- {}\n", action));
            }

            result.push_str("\n====\n\n");
            result.push_str(
                "By executing the above tools, you have gathered the following information:\n\n",
            );
        }

        // Resources
        result.push_str("## Resources in Memory\n\n");
        if self.loaded_resources.is_empty() {
            result.push_str("No resources loaded\n\n");
        } else {
            result.push_str("All resources are shown in their latest version. They already reflect the tools you may have used.\n\n");
            for (path, resource) in &self.loaded_resources {
                result.push_str(&format!(">>>>> RESOURCE: {}\n", path.display()));
                result.push_str(&resource.to_string());
                result.push_str("\n<<<<< END RESOURCE\n\n");
            }
        }

        // File tree
        result.push_str("## File Tree\n\n");
        if let Some(tree) = &self.file_tree {
            result
                .push_str("This is the file tree showing directories expanded via list_files:\n\n");
            result.push_str(&tree.to_string());
        } else {
            result.push_str("No file tree available\n");
        }
        result.push_str("\n\n");

        // Summaries
        result.push_str("## Summaries\n\n");
        if self.summaries.is_empty() {
            result.push_str("No summaries created\n");
        } else {
            result.push_str("The following resources were previously loaded, but you have decided to keep a summary only:\n\n");
            for (path, summary) in &self.summaries {
                result.push_str(&format!("- `{}`: {}\n", path.display(), summary));
            }
        }

        result
    }
    /// Add a new resource to working memory
    pub fn add_resource(&mut self, path: PathBuf, resource: LoadedResource) {
        self.loaded_resources.insert(path, resource);
    }

    /// Update an existing resource if it exists
    pub fn update_resource(&mut self, path: &PathBuf, resource: LoadedResource) -> bool {
        if self.loaded_resources.contains_key(path) {
            self.loaded_resources.insert(path.clone(), resource);
            true
        } else {
            false
        }
    }
}

/// Details for a text replacement operation
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileReplacement {
    /// The text to search for. Must match exactly one location in the file.
    pub search: String,
    /// The text to replace it with
    pub replace: String,
}

/// Available tools the agent can use
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "tool", content = "params")]
pub enum Tool {
    /// Input given by the user after the LLM has not called any tool
    UserInput,
    /// List available projects
    ListProjects,
    /// Open a project by name
    OpenProject { name: String },
    /// Update the plan
    UpdatePlan { plan: String },
    /// Delete one or more files
    DeleteFiles { paths: Vec<PathBuf> },
    /// List contents of directories
    ListFiles {
        paths: Vec<PathBuf>,
        // Optional depth limit, None means unlimited
        max_depth: Option<usize>,
    },
    /// Read content of one or multiple files into working memory
    ReadFiles { paths: Vec<PathBuf> },
    /// Write content to a file
    WriteFile {
        path: PathBuf,
        content: String,
        append: bool,
    },
    /// Replace parts within a file. Each search text must match exactly once.
    /// Returns an error if any search text matches zero or multiple times.
    ReplaceInFile {
        path: PathBuf,
        replacements: Vec<FileReplacement>,
    },
    /// Replace contents of resources with summaries in working memory
    Summarize { resources: Vec<(PathBuf, String)> },
    /// Complete the current task
    CompleteTask { message: String },
    /// Execute a CLI command
    ExecuteCommand {
        /// The complete command line to execute
        command_line: String,
        /// Optional working directory for the command
        working_dir: Option<PathBuf>,
    },
    /// Search for text in files
    SearchFiles {
        /// The text to search for in regex syntax
        regex: String,
    },
    /// Web search using DuckDuckGo
    WebSearch {
        query: String,
        hits_page_number: u32,
    },
    /// Fetch and extract content from a web page
    WebFetch {
        url: String,
        selectors: Option<Vec<String>>,
    },
}

/// Specific results for each tool type
#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum ToolResult {
    UserInput {
        message: String,
    },
    ListProjects {
        projects: HashMap<String, Project>,
    },
    OpenProject {
        name: String,
        path: Option<PathBuf>,
        error: Option<String>,
    },
    UpdatePlan {
        plan: String,
    },
    AbsolutePathError {
        path: PathBuf,
    },
    ReadFiles {
        loaded_files: HashMap<PathBuf, String>,
        failed_files: Vec<(PathBuf, String)>,
    },
    ListFiles {
        expanded_paths: Vec<(PathBuf, FileTreeEntry)>,
        failed_paths: Vec<(String, String)>,
    },
    SearchFiles {
        results: Vec<SearchResult>,
        regex: String,
    },
    ExecuteCommand {
        output: String,
        success: bool,
    },
    WriteFile {
        path: PathBuf,
        content: String,
        error: Option<String>,
    },
    ReplaceInFile {
        path: PathBuf,
        content: String,
        error: Option<crate::utils::FileUpdaterError>,
    },
    DeleteFiles {
        deleted: Vec<PathBuf>,
        failed: Vec<(PathBuf, String)>,
    },
    Summarize {
        resources: Vec<(PathBuf, String)>,
    },
    CompleteTask {
        result: String,
    },
    WebSearch {
        query: String,
        results: Vec<WebSearchResult>,
        error: Option<String>,
    },
    WebFetch {
        page: WebPage,
        error: Option<String>,
    },
}

/// Collection of all available tool definitions
#[derive(Debug, Clone)]
pub struct Tools;

/// Tool description for LLM
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("Unknown tool: {0}")]
    UnknownTool(String),

    #[error("Failed to parse tool parameters: {0}")]
    ParseError(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Represents the parsed response from the LLM
#[derive(Debug, Deserialize)]
pub struct AgentAction {
    pub tool: Tool,
    pub reasoning: String,
    pub tool_id: String, // ID of the tool for UI status tracking
}

/// Result of a tool execution
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ActionResult {
    pub tool: Tool,
    pub result: ToolResult,
    pub reasoning: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("LLM error: {0}")]
    LLMError(#[from] anyhow::Error),

    #[error("Action error: {error}")]
    ActionError {
        error: anyhow::Error,
        message: Message,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileSystemEntry {
    pub path: PathBuf,
    pub name: String,
    pub entry_type: FileSystemEntryType,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum FileSystemEntryType {
    File,
    Directory,
}

#[derive(Debug, Clone)]
pub enum SearchMode {
    /// Standard text search, case-insensitive by default
    Exact,
    /// Regular expression search
    Regex,
}

impl Default for SearchMode {
    fn default() -> Self {
        Self::Exact
    }
}

#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    pub query: String,
    pub case_sensitive: bool,
    pub whole_words: bool,
    pub mode: SearchMode,
    pub max_results: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchResult {
    pub file: PathBuf,
    pub start_line: usize, // First line in the section (including context)
    pub line_content: Vec<String>, // All lines in the section
    pub match_lines: Vec<usize>, // Line numbers with matches (relative to start_line)
    pub match_ranges: Vec<Vec<(usize, usize)>>, // Match positions for each line, aligned with match_lines
}

pub trait CodeExplorer: Send + Sync {
    fn root_dir(&self) -> PathBuf;
    /// Reads the content of a file
    fn read_file(&self, path: &PathBuf) -> Result<String>;
    /// Write the content of a file
    fn write_file(&self, path: &PathBuf, content: &String, append: bool) -> Result<()>;
    fn delete_file(&self, path: &PathBuf) -> Result<()>;
    fn create_initial_tree(&mut self, max_depth: usize) -> Result<FileTreeEntry>;
    fn list_files(&mut self, path: &PathBuf, max_depth: Option<usize>) -> Result<FileTreeEntry>;
    /// Applies FileReplacements to a file
    fn apply_replacements(&self, path: &Path, replacements: &[FileReplacement]) -> Result<String>;
    /// Search for text in files with advanced options
    fn search(&self, path: &Path, options: SearchOptions) -> Result<Vec<SearchResult>>;
}
