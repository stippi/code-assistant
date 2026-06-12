/// Trait for determining whether a tool execution was successful
pub trait ToolResult: Send + Sync + 'static {
    /// Returns whether the tool execution was successful
    /// This is used for status reporting and can affect how the result is displayed
    fn is_success(&self) -> bool;
}

/// Errors surfaced when resolving or invoking a tool
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("Unknown tool: {0}")]
    UnknownTool(String),

    #[error("Failed to parse tool parameters: {0}")]
    ParseError(String),
}
