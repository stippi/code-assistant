use std::collections::HashMap;
use std::sync::OnceLock;

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

    /// Register a tool in the registry
    pub fn register(&mut self, tool: Box<dyn DynTool>) {
        self.tools.insert(tool.spec().name.to_string(), tool);
    }

    /// Get a tool by name
    pub fn get(&self, name: &str) -> Option<&Box<dyn DynTool>> {
        self.tools.get(name)
    }

    /// Check if a tool is hidden by consulting the tool definitions
    pub fn is_tool_hidden(&self, tool_name: &str, scope: ToolScope) -> bool {
        self.tools
            .values()
            .filter(|tool| tool.spec().supported_scopes.contains(&scope))
            .find(|tool| tool.spec().name == tool_name)
            .map(|tool| tool.spec().hidden)
            .unwrap_or(false)
    }

    /// Get tool definitions for a specific mode
    pub fn get_tool_definitions_for_scope(&self, mode: ToolScope) -> Vec<AnnotatedToolDefinition> {
        self.tools
            .values()
            .filter(|tool| tool.spec().supported_scopes.contains(&mode))
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
            SearchFilesTool, WebFetchTool, WebSearchTool, WriteFileTool,
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
        self.register(Box::new(WebFetchTool));
        self.register(Box::new(WebSearchTool));
        self.register(Box::new(WriteFileTool));

        // More tools will be added here as they are implemented
    }
}
