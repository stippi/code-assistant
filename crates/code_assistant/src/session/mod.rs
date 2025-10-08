use crate::agent::ToolExecution;
use crate::persistence::LlmSessionConfig;
use crate::types::ToolSyntax;
use llm::Message;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// New session management architecture
pub mod instance;
pub mod manager;

// Main session manager
pub use manager::SessionManager;

/// Static configuration stored with each session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_path: Option<PathBuf>,
    #[serde(default)]
    pub initial_project: String,
    #[serde(default = "default_tool_syntax")]
    pub tool_syntax: ToolSyntax,
    #[serde(default)]
    pub use_diff_blocks: bool,
}

fn default_tool_syntax() -> ToolSyntax {
    ToolSyntax::Native
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            init_path: None,
            initial_project: String::new(),
            tool_syntax: default_tool_syntax(),
            use_diff_blocks: false,
        }
    }
}

/// State data needed to restore an agent session
#[derive(Debug, Clone)]
pub struct SessionState {
    pub session_id: String,
    pub name: String,
    pub messages: Vec<Message>,
    pub tool_executions: Vec<ToolExecution>,
    pub working_memory: crate::types::WorkingMemory,
    pub config: SessionConfig,
    pub next_request_id: Option<u64>,
    pub llm_config: Option<LlmSessionConfig>,
}
