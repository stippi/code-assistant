/// Trait for determining whether a tool execution was successful
pub trait ToolResult: Send + Sync + 'static {
    /// Returns whether the tool execution was successful
    /// This is used for status reporting and can affect how the result is displayed
    fn is_success(&self) -> bool;
}
