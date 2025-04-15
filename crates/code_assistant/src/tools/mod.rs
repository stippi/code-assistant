use crate::types::ToolResult;
use anyhow::Result;

mod definitions;
mod executor;
mod handlers;
mod parse;
mod result;
mod types;

pub use executor::ToolExecutor;
pub use handlers::{AgentChatToolHandler, AgentToolHandler, MCPToolHandler};
pub use parse::{parse_tool_json, parse_tool_xml, TOOL_TAG_PREFIX};
pub use types::AnnotatedToolDefinition;

#[async_trait::async_trait]
pub trait ToolResultHandler: Send + Sync {
    /// Handle a tool result, update internal state if needed, and return formatted output
    async fn handle_result(&mut self, result: &ToolResult) -> Result<String>;
}
