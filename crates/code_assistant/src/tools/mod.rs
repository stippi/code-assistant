use crate::types::ToolResult;
use anyhow::Result;

// Original tools implementation
mod definitions;
mod executor;
mod handlers;
mod parse;
mod result;
mod types;

// New trait-based tools implementation
pub mod core;
pub mod impls;

#[cfg(test)]
mod tests;

pub use parse::{parse_tool_xml, TOOL_TAG_PREFIX};
pub use types::AnnotatedToolDefinition;

#[async_trait::async_trait]
pub trait ToolResultHandler: Send + Sync {
    /// Handle a tool result, update internal state if needed, and return formatted output
    async fn handle_result(&mut self, result: &ToolResult) -> Result<String>;
}
