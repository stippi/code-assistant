// Parsing helpers for tool inputs (paths with ranges, search/replace blocks)
pub mod parse;

// Tool use filtering system
pub mod tool_use_filter;

// Tools configuration (tools.json)
pub mod config;

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

// The loop-side tool vocabulary moved to the agent core (Phase 4 step 2).
pub use agent_core::ToolRequest;


use crate::tools::core::{ToolRegistry, ToolsConfig};
use std::sync::{Arc, OnceLock};

static GLOBAL_REGISTRY: OnceLock<Arc<ToolRegistry>> = OnceLock::new();

fn global_registry_cell() -> &'static Arc<ToolRegistry> {
    GLOBAL_REGISTRY.get_or_init(|| {
        let mut registry = ToolRegistry::new();
        register_default_tools(&mut registry);
        Arc::new(registry)
    })
}

/// The process-wide registry preloaded with code-assistant's default tools.
///
/// Scheduled for removal in Phase 6 of the extraction plan; prefer passing a
/// registry instance where feasible.
pub fn global_registry() -> &'static ToolRegistry {
    global_registry_cell()
}

/// Shared-handle variant of [`global_registry`], for components that store
/// the registry (e.g. the agent runtime).
pub fn global_registry_arc() -> Arc<ToolRegistry> {
    global_registry_cell().clone()
}

/// Register all of code-assistant's tools in the given registry. Tools that
/// depend on external services are skipped when their configuration is
/// missing.
pub fn register_default_tools(registry: &mut ToolRegistry) {
    use impls::{
        DeleteFilesTool, EditTool, ExecuteCommandTool, GlobFilesTool, ListFilesTool,
        ListProjectsTool, NameSessionTool, PerplexityAskTool, ReadFilesTool, ReplaceInFileTool,
        SearchFilesTool, SpawnAgentTool, UpdatePlanTool, ViewDocumentsTool, ViewImagesTool,
        WebFetchTool, WebSearchTool, WriteFileTool,
    };

    let config = ToolsConfig::global();

    registry.register(Box::new(DeleteFilesTool));
    registry.register(Box::new(EditTool));
    registry.register(Box::new(ExecuteCommandTool));
    registry.register(Box::new(GlobFilesTool));
    registry.register(Box::new(ListFilesTool));
    registry.register(Box::new(ListProjectsTool));
    registry.register(Box::new(NameSessionTool));
    if PerplexityAskTool.is_available(config) {
        registry.register(Box::new(PerplexityAskTool));
    } else {
        tracing::debug!("Tool 'perplexity_ask' is not available (missing configuration)");
    }
    registry.register(Box::new(ReadFilesTool));
    registry.register(Box::new(ReplaceInFileTool));
    registry.register(Box::new(SearchFilesTool));
    registry.register(Box::new(SpawnAgentTool));
    registry.register(Box::new(UpdatePlanTool));
    registry.register(Box::new(ViewDocumentsTool));
    registry.register(Box::new(ViewImagesTool));
    registry.register(Box::new(WebFetchTool));
    registry.register(Box::new(WebSearchTool));
    registry.register(Box::new(WriteFileTool));
}
