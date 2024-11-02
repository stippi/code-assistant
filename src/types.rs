use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Represents the agent's working memory during execution
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct WorkingMemory {
    /// Currently loaded file contents
    pub loaded_files: HashMap<PathBuf, String>,
    /// Summaries of previously seen files
    pub file_summaries: HashMap<PathBuf, String>,
    /// Current task description
    pub current_task: String,
    /// Memory of previous actions and their results
    pub action_history: Vec<ActionResult>,
    /// Additional context or notes the agent has generated
    pub notes: Vec<String>,
}

/// Available tools the agent can use
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "tool", content = "params")]
pub enum Tool {
    /// List files in a directory
    ListFiles { path: PathBuf },
    /// Read content of a specific file
    ReadFile { path: PathBuf },
    /// Write content to a file
    WriteFile { path: PathBuf, content: String },
    /// Remove content from working memory to free up space
    UnloadFile { path: PathBuf },
    /// Add a summary to working memory
    AddSummary { path: PathBuf, summary: String },
}

/// Result of a tool execution
#[derive(Debug, Serialize, Deserialize)]
pub struct ActionResult {
    pub tool: Tool,
    pub success: bool,
    pub result: String,
    pub error: Option<String>,
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
#[derive(Debug)]
pub struct AgentAction {
    pub tool: Tool,
    pub reasoning: String,
    pub task_completed: bool,
}
