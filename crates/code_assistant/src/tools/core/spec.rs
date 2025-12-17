/// Define available modes for tools
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolScope {
    /// Tool can be used in the MCP server
    McpServer,
    /// Tool can be used in the message history agent
    Agent,
    /// Tool can be used in the agent when configured for diff blocks format
    AgentWithDiffBlocks,
    /// Tool scope for sub-agents running in a restricted, read-only mode
    SubAgentReadOnly,
    /// Tool scope for sub-agents running with broader permissions (reserved for future use)
    SubAgentDefault,
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
    /// Whether this tool should be hidden from UI display
    pub hidden: bool,
    /// Optional template for generating dynamic titles from parameters
    /// Use {parameter_name} placeholders, e.g. "Reading {paths}" or "Searching for '{regex}'"
    pub title_template: Option<&'static str>,
}
