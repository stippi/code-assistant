pub mod acp;
pub mod gpui;
pub mod server;
pub mod terminal;

use crate::types::ToolSyntax;

use std::path::PathBuf;

/// Configuration for running the agent in either terminal or GPUI mode
#[derive(Debug, Clone)]
pub struct AgentRunConfig {
    pub path: PathBuf,
    pub task: Option<String>,
    pub continue_task: bool,
    pub model: String,
    pub tool_syntax: ToolSyntax,
    pub use_diff_format: bool,
    pub record: Option<PathBuf>,
    pub playback: Option<PathBuf>,
    pub fast_playback: bool,
}
