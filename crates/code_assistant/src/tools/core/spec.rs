/// Define available modes for tools.
///
/// This is selection vocabulary, not tool metadata: each scope maps to a
/// capability tag (see [`ToolScope::tag`]) that tools carry in their
/// [`ToolSpec::capabilities`].
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
    /// Tool scope for sub-agents running with broader permissions
    SubAgentDefault,
    /// Same as `SubAgentDefault` but with the diff-format edit tool
    /// (`replace_in_file`) instead of the simple `edit` tool. Selected by
    /// the sub-agent runner when the parent session has
    /// `use_diff_blocks = true`, so sub-agents inherit their parent's
    /// edit-tool layout.
    SubAgentDefaultWithDiffBlocks,
}

impl ToolScope {
    /// The capability tag marking a tool as offered in this scope.
    pub fn tag(&self) -> &'static str {
        match self {
            ToolScope::McpServer => capabilities::SCOPE_MCP,
            ToolScope::Agent => capabilities::SCOPE_AGENT,
            ToolScope::AgentWithDiffBlocks => capabilities::SCOPE_AGENT_DIFF,
            ToolScope::SubAgentReadOnly => capabilities::SCOPE_SUBAGENT_READ_ONLY,
            ToolScope::SubAgentDefault => capabilities::SCOPE_SUBAGENT_DEFAULT,
            ToolScope::SubAgentDefaultWithDiffBlocks => capabilities::SCOPE_SUBAGENT_DEFAULT_DIFF,
        }
    }
}

/// Capability tags describing what a tool may do and where it is offered.
/// Free-form strings; the constants below cover the tags code-assistant
/// itself evaluates.
pub mod capabilities {
    /// The tool does not modify any state. Read-only tools are safe to chain
    /// within a single assistant turn.
    pub const READ_ONLY: &str = "read_only";
    /// The tool modifies files in a project.
    pub const EDITS_FILES: &str = "edits_files";

    /// Scope tags: where a tool is offered (see `ToolScope::tag`).
    pub const SCOPE_MCP: &str = "scope:mcp";
    pub const SCOPE_AGENT: &str = "scope:agent";
    pub const SCOPE_AGENT_DIFF: &str = "scope:agent-diff";
    pub const SCOPE_SUBAGENT_READ_ONLY: &str = "scope:subagent-read-only";
    pub const SCOPE_SUBAGENT_DEFAULT: &str = "scope:subagent-default";
    pub const SCOPE_SUBAGENT_DEFAULT_DIFF: &str = "scope:subagent-default-diff";
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
    /// Capability tags (see [`capabilities`]) consumers select tools by,
    /// including the scope tags saying where the tool is offered
    pub capabilities: &'static [&'static str],
    /// Parameters whose values typically span multiple lines. Text dialects
    /// (XML, Caret) render these with block syntax; native tool calling
    /// ignores this.
    pub multiline_params: &'static [&'static str],
    /// Whether this tool should be hidden from UI display
    pub hidden: bool,
    /// Optional template for generating dynamic titles from parameters
    /// Use {parameter_name} placeholders, e.g. "Reading {paths}" or "Searching for '{regex}'"
    pub title_template: Option<&'static str>,
}

impl ToolSpec {
    pub fn has_capability(&self, capability: &str) -> bool {
        self.capabilities.contains(&capability)
    }

    pub fn is_multiline_param(&self, name: &str) -> bool {
        self.multiline_params.contains(&name)
    }
}
