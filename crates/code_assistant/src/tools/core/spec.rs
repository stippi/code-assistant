/// Define available modes for tools
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolScope {
    /// Tool can be used in the MCP server
    McpServer,
    /// Tool can be used in the message history agent
    Agent,
}

/// Specification for a tool, including metadata
#[derive(Clone)]
pub struct ToolSpec {
    /// Unique name of the tool
    pub name: &'static str,
    /// Detailed description of what the tool does
    pub description: &'static str,
    /// JSON Schema for the tool's parameters
    pub parameters_schema: serde_json::Value,
    /// Optional annotations for LLM-specific instructions
    pub annotations: Option<serde_json::Value>,
    /// Which execution modes this tool supports
    pub supported_scopes: &'static [ToolScope],
}
