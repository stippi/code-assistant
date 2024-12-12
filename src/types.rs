use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
pub struct FileUpdate {
    pub start_line: usize,
    pub end_line: usize,
    pub new_content: String,
}

/// Available tools the agent can use
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "tool", content = "params")]
pub enum Tool {
    /// Delete one or more files
    DeleteFiles { paths: Vec<PathBuf> },
    /// List contents of directories
    ListFiles {
        paths: Vec<PathBuf>,
        // Optional depth limit, None means unlimited
        max_depth: Option<usize>,
    },
    /// Read content of one or multiple files
    ReadFiles { paths: Vec<PathBuf> },
    /// Write content to a file
    WriteFile { path: PathBuf, content: String },
    /// Update parts of a file
    UpdateFile {
        path: PathBuf,
        updates: Vec<FileUpdate>,
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
}

/// Result of a tool execution
#[derive(Debug, Serialize, Deserialize)]
pub struct ActionResult {
    pub tool: Tool,
    pub success: bool,
    pub result: String,
    pub error: Option<String>,
    pub reasoning: String,
}

/// Agent's response after processing
#[derive(Debug, Serialize, Deserialize)]
pub struct AgentResponse {
    /// Next tool to execute
    pub next_action: Tool,
    /// Agent's reasoning for choosing this action
    pub reasoning: String,
    /// Whether the agent believes the task is completed
    pub task_completed: bool,
}

/// LLM request structure
#[derive(Debug, Serialize)]
pub struct LLMRequest {
    pub task: String,
    pub working_memory: WorkingMemory,
    pub available_tools: Vec<ToolDescription>,
    pub max_tokens: usize,
}

/// Tool description for LLM
#[derive(Debug, Serialize)]
pub struct ToolDescription {
    pub name: String,
    pub description: String,
    pub parameters: HashMap<String, String>,
}

/// Represents the parsed response from the LLM
#[derive(Debug, Deserialize)]
pub struct AgentAction {
    pub tool: Tool,
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

pub trait CodeExplorer {
    fn root_dir(&self) -> PathBuf;
    /// Reads the content of a file
    fn read_file(&self, path: &PathBuf) -> Result<String>;
    fn create_initial_tree(&self, max_depth: usize) -> Result<FileTreeEntry>;
    fn list_files(&self, path: &PathBuf, max_depth: Option<usize>) -> Result<FileTreeEntry>;
    /// Applies FileUpdates to a file
    fn apply_updates(&self, path: &Path, updates: &[FileUpdate]) -> Result<String>;
}
