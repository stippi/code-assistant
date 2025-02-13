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

/// Represents the agent's working memory during execution
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct WorkingMemory {
    /// Currently loaded file contents
    pub loaded_files: HashMap<PathBuf, String>,
    /// Summaries of previously seen files
    pub file_summaries: HashMap<PathBuf, String>,
    /// Complete file tree of the repository
    pub file_tree: Option<FileTreeEntry>,
    /// Current task description
    pub current_task: String,
    /// Memory of previous actions and their results
    pub action_history: Vec<ActionResult>,
    /// Additional context or notes the agent has generated
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileReplacement {
    pub search: String,
    pub replace: String,
}

/// Available tools the agent can use
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "tool", content = "params")]
pub enum Tool {
    /// List available projects
    ListProjects,
    /// Open a project by name
    OpenProject { name: String },
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
    WriteFile { path: PathBuf, content: String },
    /// Replace parts within a file
    ReplaceInFile {
        path: PathBuf,
        replacements: Vec<FileReplacement>,
    },
    /// Replace file content with summaries in working memory
    Summarize { files: Vec<(PathBuf, String)> },
    /// Ask user a question and wait for response
    AskUser { question: String },
    /// Message the user
    MessageUser { message: String },
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
        /// The text to search for
        query: String,
        /// Optional directory path to search in
        path: Option<PathBuf>,
        /// Whether the search should be case-sensitive
        case_sensitive: bool,
        /// Whether to match whole words only
        whole_words: bool,
        /// Whether to use regex mode
        regex_mode: bool,
        /// Maximum number of results to return
        max_results: Option<usize>,
    },
}

/// Specific results for each tool type
#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum ToolResult {
    ListProjects {
        projects: HashMap<String, Project>,
    },
    OpenProject {
        name: String,
        path: Option<PathBuf>,
        error: Option<String>,
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
        query: String,
    },
    ExecuteCommand {
        stdout: String,
        stderr: String,
        error: Option<String>,
    },
    WriteFile {
        path: PathBuf,
        content: String,
        error: Option<String>,
    },
    ReplaceInFile {
        path: PathBuf,
        content: String,
        error: Option<String>,
    },
    DeleteFiles {
        deleted: Vec<PathBuf>,
        failed: Vec<(PathBuf, String)>,
    },
    Summarize {
        files: Vec<(PathBuf, String)>,
    },
    AskUser {
        response: String,
    },
    MessageUser {
        result: String,
    },
    CompleteTask {
        result: String,
    },
}

/// Collection of all available tool definitions
#[derive(Debug, Clone)]
pub struct Tools;

/// Tool description for LLM
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
}

/// Result of a tool execution
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ActionResult {
    pub tool: Tool,
    pub result: ToolResult,
    pub reasoning: String,
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
    pub line_number: usize,
    pub line_content: String,
    pub match_ranges: Vec<(usize, usize)>, // Start and end positions of matches in the line
}

pub trait CodeExplorer: Send + Sync {
    fn root_dir(&self) -> PathBuf;
    /// Reads the content of a file
    fn read_file(&self, path: &PathBuf) -> Result<String>;
    /// Write the content of a file
    fn write_file(&self, path: &PathBuf, content: &String) -> Result<()>;
    fn delete_file(&self, path: &PathBuf) -> Result<()>;
    fn create_initial_tree(&mut self, max_depth: usize) -> Result<FileTreeEntry>;
    fn list_files(&mut self, path: &PathBuf, max_depth: Option<usize>) -> Result<FileTreeEntry>;
    /// Applies FileReplacements to a file
    fn apply_replacements(&self, path: &Path, replacements: &[FileReplacement]) -> Result<String>;
    /// Search for text in files with advanced options
    fn search(&self, path: &Path, options: SearchOptions) -> Result<Vec<SearchResult>>;
}
