use std::collections::HashMap;
use std::sync::OnceLock;

use crate::tools::core::config::ToolsConfig;
use crate::tools::core::dyn_tool::DynTool;
use crate::tools::core::spec::ToolScope;
use crate::tools::AnnotatedToolDefinition;

/// Central registry for all tools in the system
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn DynTool>>,
}

impl ToolRegistry {
    /// Get the global singleton instance of the registry
    pub fn global() -> &'static Self {
        // Singleton instance of the registry
        static INSTANCE: OnceLock<ToolRegistry> = OnceLock::new();
        INSTANCE.get_or_init(|| {
            let mut registry = ToolRegistry::new();
            registry.register_default_tools();
            registry
        })
    }

    /// Create a new empty registry
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool in the registry.
    /// The tool will only be registered if it's available based on the current configuration.
    pub fn register(&mut self, tool: Box<dyn DynTool>) {
        let config = ToolsConfig::global();
        if tool.is_available(config) {
            self.tools.insert(tool.spec().name.to_string(), tool);
        } else {
            tracing::debug!(
                "Tool '{}' is not available (missing configuration)",
                tool.spec().name
            );
        }
    }

    /// Get a tool by name
    pub fn get(&self, name: &str) -> Option<&dyn DynTool> {
        self.tools.get(name).map(|boxed| boxed.as_ref())
    }

    /// Check if the named tool carries the given capability tag.
    /// Unknown tools have no capabilities.
    pub fn tool_has_capability(&self, tool_name: &str, capability: &str) -> bool {
        self.tools
            .get(tool_name)
            .map(|tool| tool.spec().has_capability(capability))
            .unwrap_or(false)
    }

    /// Check if a tool is allowed in the given scope
    pub fn is_tool_in_scope(&self, tool_name: &str, scope: ToolScope) -> bool {
        self.tool_has_capability(tool_name, scope.tag())
    }

    /// A shareable predicate over [`Self::is_tool_hidden`] for the given scope.
    /// Lets UI-layer consumers check hidden-ness without referencing the registry.
    pub fn hidden_tools(
        &'static self,
        scope: ToolScope,
    ) -> std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync> {
        std::sync::Arc::new(move |name| self.is_tool_hidden(name, scope))
    }

    /// Check if a tool is hidden by consulting the tool definitions
    pub fn is_tool_hidden(&self, tool_name: &str, scope: ToolScope) -> bool {
        self.tools
            .values()
            .filter(|tool| tool.spec().has_capability(scope.tag()))
            .find(|tool| tool.spec().name == tool_name)
            .map(|tool| tool.spec().hidden)
            .unwrap_or(false)
    }

    /// Get tool definitions for a specific mode
    pub fn get_tool_definitions_for_scope(&self, mode: ToolScope) -> Vec<AnnotatedToolDefinition> {
        self.tools
            .values()
            .filter(|tool| tool.spec().has_capability(mode.tag()))
            .map(|tool| {
                let spec = tool.spec();
                AnnotatedToolDefinition {
                    name: spec.name.to_string(),
                    description: spec.description.to_string(),
                    parameters: spec.parameters_schema.clone(),
                    annotations: spec.annotations.clone(),
                }
            })
            .collect()
    }

    /// Register all default tools in the system
    /// This will be expanded as we implement more tools
    fn register_default_tools(&mut self) {
        // Import all tools
        use crate::tools::impls::{
            DeleteFilesTool, EditTool, ExecuteCommandTool, GlobFilesTool, ListFilesTool,
            ListProjectsTool, NameSessionTool, PerplexityAskTool, ReadFilesTool, ReplaceInFileTool,
            SearchFilesTool, SpawnAgentTool, UpdatePlanTool, ViewDocumentsTool, ViewImagesTool,
            WebFetchTool, WebSearchTool, WriteFileTool,
        };

        // Register all tools - the ToolScope system will filter which ones are available
        self.register(Box::new(DeleteFilesTool));
        self.register(Box::new(EditTool));
        self.register(Box::new(ExecuteCommandTool));
        self.register(Box::new(GlobFilesTool));
        self.register(Box::new(ListFilesTool));
        self.register(Box::new(ListProjectsTool));
        self.register(Box::new(NameSessionTool));
        self.register(Box::new(PerplexityAskTool));
        self.register(Box::new(ReadFilesTool));
        self.register(Box::new(ReplaceInFileTool));
        self.register(Box::new(SearchFilesTool));
        self.register(Box::new(SpawnAgentTool));
        self.register(Box::new(UpdatePlanTool));
        self.register(Box::new(ViewDocumentsTool));
        self.register(Box::new(ViewImagesTool));
        self.register(Box::new(WebFetchTool));
        self.register(Box::new(WebSearchTool));
        self.register(Box::new(WriteFileTool));

        // More tools will be added here as they are implemented
    }
}
