// Original tools implementation
mod parse;
mod types;

// Parser registry for different tool syntaxes
pub mod parser_registry;

// System message generation
pub mod system_message;

// Tool use filtering system
pub mod tool_use_filter;

// Tool formatter system
pub mod formatter;

// New trait-based tools implementation
pub mod core;
pub mod impls;

// Application services handed to tools through ToolContext::extensions
pub mod services;

// code-assistant's tool selection vocabulary (ToolScope, scope:* tags)
pub mod scope;

#[cfg(test)]
mod tests;

pub(crate) use parse::parse_search_replace_blocks;
pub use parse::{parse_caret_tool_invocations, parse_xml_tool_invocations};
pub use parser_registry::ParserRegistry;
pub use services::{ToolServices, ToolServicesAccess};
pub use system_message::generate_system_message;
pub use types::{to_tool_definitions, ParseError, PromptTooLongError, ToolRequest};


use crate::tools::core::{ToolRegistry, ToolsConfig};
use std::sync::OnceLock;

/// The process-wide registry preloaded with code-assistant's default tools.
///
/// Scheduled for removal in Phase 6 of the extraction plan; prefer passing a
/// registry instance where feasible.
pub fn global_registry() -> &'static ToolRegistry {
    static INSTANCE: OnceLock<ToolRegistry> = OnceLock::new();
    INSTANCE.get_or_init(|| {
        let mut registry = ToolRegistry::new();
        register_default_tools(&mut registry);
        registry
    })
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
