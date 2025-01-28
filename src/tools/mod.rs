use crate::types::ActionResult;
use anyhow::Result;

mod executor;
mod handlers;
mod parse;

pub use executor::ToolExecutor;
pub use handlers::{AgentToolHandler, MCPToolHandler, ReplayToolHandler};
pub use parse::{parse_tool_json, parse_tool_xml, TOOL_TAG_PREFIX};

#[async_trait::async_trait]
pub trait ToolResultHandler: Send + Sync {
    /// Handle a tool result, update internal state if needed, and return formatted output
    async fn handle_result(&mut self, result: &ActionResult) -> Result<String>;
}
