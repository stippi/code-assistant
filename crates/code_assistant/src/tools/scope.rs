//! code-assistant's tool selection vocabulary.
//!
//! [`ToolScope`] names the contexts in which tools are offered; each scope
//! maps to a capability tag (see [`ToolScope::tag`]) that tools carry in
//! their `ToolSpec::capabilities`. The generic tool core only knows the
//! capability tags.

/// Define available modes for tools.
///
/// This is selection vocabulary, not tool metadata: each scope maps to a
/// capability tag (see [`ToolScope::tag`]) that tools carry in their
/// `ToolSpec::capabilities`.
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

/// code-assistant's capability tags: the generic ones from the tool core
/// plus the `scope:*` tags saying where a tool is offered.
pub mod capabilities {
    pub use crate::tools::core::spec::capabilities::*;

    /// Scope tags: where a tool is offered (see `ToolScope::tag`).
    pub const SCOPE_MCP: &str = "scope:mcp";
    pub const SCOPE_AGENT: &str = "scope:agent";
    pub const SCOPE_AGENT_DIFF: &str = "scope:agent-diff";
    pub const SCOPE_SUBAGENT_READ_ONLY: &str = "scope:subagent-read-only";
    pub const SCOPE_SUBAGENT_DEFAULT: &str = "scope:subagent-default";
    pub const SCOPE_SUBAGENT_DEFAULT_DIFF: &str = "scope:subagent-default-diff";
}
