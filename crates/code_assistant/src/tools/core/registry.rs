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
            // Check environment variable for diff format preference
            let use_diff_format = std::env::var("CODE_ASSISTANT_USE_DIFF_FORMAT")
                .map(|v| v.to_lowercase() == "true")
                .unwrap_or(false);
            registry.register_default_tools_impl(use_diff_format);
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
    #[cfg(test)]
    pub fn register_default_tools(&mut self, use_diff_format: bool) {
        self.register_default_tools_impl(use_diff_format);
    }

    /// Internal implementation of register_default_tools
    fn register_default_tools_impl(&mut self, use_diff_format: bool) {
        // Import all tools
        use crate::tools::impls::{
            DeleteFilesTool, EditTool, ExecuteCommandTool, ListFilesTool, ListProjectsTool, NameSessionTool,
            PerplexityAskTool, ReadFilesTool, ReplaceInFileTool, SearchFilesTool, WebFetchTool,
            WebSearchTool, WriteFileTool,
        };

        // Register core tools
        self.register(Box::new(DeleteFilesTool));
        self.register(Box::new(ExecuteCommandTool));
        self.register(Box::new(ListFilesTool));
        self.register(Box::new(ListProjectsTool));
        self.register(Box::new(NameSessionTool));
        self.register(Box::new(PerplexityAskTool));
        self.register(Box::new(ReadFilesTool));
        self.register(Box::new(SearchFilesTool));
        self.register(Box::new(WebFetchTool));
        self.register(Box::new(WebSearchTool));
        self.register(Box::new(WriteFileTool));

        // Register file editing tools based on configuration
        if use_diff_format {
            // Use legacy diff format (replace_in_file tool)
            self.register(Box::new(ReplaceInFileTool));
        } else {
            // Use new edit tool (default)
            self.register(Box::new(EditTool));
        }

        // More tools will be added here as they are implemented
    }
}
