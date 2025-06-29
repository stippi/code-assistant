use llm::Message;
use std::path::PathBuf;

use crate::agent::ToolExecution;

// New session management architecture
pub mod instance;
pub mod manager;

// Main session manager
pub use manager::{AgentConfig, SessionManager};

/// State data needed to restore an agent session
#[derive(Debug)]
pub struct SessionState {
    pub session_id: String,
    pub messages: Vec<Message>,
    pub tool_executions: Vec<ToolExecution>,
    pub working_memory: crate::types::WorkingMemory,
    pub init_path: Option<PathBuf>,
    pub initial_project: Option<String>,
    pub next_request_id: Option<u64>,
}
