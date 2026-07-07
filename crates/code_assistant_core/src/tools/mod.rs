// Parsing helpers for tool inputs (paths with ranges, search/replace blocks)
pub mod parse;

// Tool use filtering system
pub mod tool_use_filter;

// Tools configuration (tools.json)
pub mod config;

// MCP client mode: mcp-servers.json + registration of MCP server tools
pub mod mcp;

// New trait-based tools implementation
pub mod core;
pub mod impls;

// Application services handed to tools through ToolContext::extensions
pub mod services;

// code-assistant's tool selection vocabulary (ToolScope, scope:* tags)
pub mod scope;

#[cfg(test)]
mod tests;

pub use parse::parse_search_replace_blocks;
pub use services::{ToolServices, ToolServicesAccess};

// The loop-side tool vocabulary lives in the agent core.
pub use agent_core::ToolRequest;

use crate::tools::core::{ToolRegistry, ToolsConfig};
use std::sync::Arc;

/// Build a registry with code-assistant's default tools, loading the tools
/// configuration (`tools.json`) from disk. Intended for the wiring layer:
/// create one per process entry point and share the `Arc`.
pub fn default_registry() -> Arc<ToolRegistry> {
    let config = ToolsConfig::load().unwrap_or_default();
    let mut registry = ToolRegistry::new();
    register_default_tools(&mut registry, &config);
    Arc::new(registry)
}

/// [`default_registry`] plus the tools of all MCP servers configured in
/// `mcp-servers.json`. Connecting to the servers is asynchronous (child
/// processes, initialize handshake), hence the async variant; wiring layers
/// without a config or without enabled servers pay nothing.
pub async fn default_registry_with_mcp() -> Arc<ToolRegistry> {
    let config = ToolsConfig::load().unwrap_or_default();
    let mut registry = ToolRegistry::new();
    register_default_tools(&mut registry, &config);
    mcp::register_configured_mcp_tools(&mut registry).await;
    Arc::new(registry)
}

/// Registry with code-assistant's default tools and an empty tools
/// configuration — a deterministic fixture for tests (no `tools.json`
/// influence, so e.g. `perplexity_ask` is never registered).
#[cfg(any(test, feature = "test-utils"))]
pub fn test_registry() -> Arc<ToolRegistry> {
    let mut registry = ToolRegistry::new();
    register_default_tools(&mut registry, &ToolsConfig::default());
    Arc::new(registry)
}

/// Register all of code-assistant's tools in the given registry. Tools that
/// depend on external services are skipped when their configuration is
/// missing.
pub fn register_default_tools(registry: &mut ToolRegistry, config: &ToolsConfig) {
    use impls::{
        CancelWakeupTool, DeleteFilesTool, EditTool, ExecuteCommandTool, GlobFilesTool,
        ListFilesTool, ListProjectsTool, ListSkillsTool, NameSessionTool, PerplexityAskTool,
        ReadFilesTool, ReadSkillTool, ReplaceInFileTool, ScheduleWakeupTool, SearchFilesTool,
        SpawnAgentTool, UpdatePlanTool, ViewDocumentsTool, ViewImagesTool, WebFetchTool,
        WebSearchTool, WriteFileTool, WriteStdinTool,
    };

    registry.register(Box::new(DeleteFilesTool));
    registry.register(Box::new(EditTool));
    registry.register(Box::new(ExecuteCommandTool));
    registry.register(Box::new(GlobFilesTool));
    registry.register(Box::new(ListFilesTool));

    registry.register(Box::new(ListProjectsTool));
    registry.register(Box::new(ListSkillsTool));
    registry.register(Box::new(NameSessionTool));
    if let Some(perplexity) = PerplexityAskTool::from_config(config) {
        registry.register(Box::new(perplexity));
    } else {
        tracing::debug!("Tool 'perplexity_ask' is not available (missing configuration)");
    }
    registry.register(Box::new(ReadFilesTool));
    registry.register(Box::new(ReadSkillTool));
    registry.register(Box::new(ReplaceInFileTool));
    registry.register(Box::new(SearchFilesTool));
    registry.register(Box::new(ScheduleWakeupTool));
    registry.register(Box::new(CancelWakeupTool));
    registry.register(Box::new(SpawnAgentTool));
    registry.register(Box::new(UpdatePlanTool));
    registry.register(Box::new(ViewDocumentsTool));
    registry.register(Box::new(ViewImagesTool));
    registry.register(Box::new(WebFetchTool));
    registry.register(Box::new(WebSearchTool));
    registry.register(Box::new(WriteFileTool));
    registry.register(Box::new(WriteStdinTool));
}
