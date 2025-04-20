use std::collections::HashMap;
use std::sync::OnceLock;

use crate::tools::core::dyn_tool::DynTool;
use crate::tools::core::spec::ToolMode;
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
        Self { tools: HashMap::new() }
    }

    /// Register a tool in the registry
    pub fn register(&mut self, tool: Box<dyn DynTool>) {
        self.tools.insert(tool.spec().name.to_string(), tool);
    }

    /// Get a tool by name
    pub fn get(&self, name: &str) -> Option<&Box<dyn DynTool>> {
        self.tools.get(name)
    }

    /// Get all registered tools
    pub fn all(&self) -> Vec<&Box<dyn DynTool>> {
        self.tools.values().collect()
    }

    /// Get tools available for a specific mode
    pub fn tools_for_mode(&self, mode: ToolMode) -> Vec<&Box<dyn DynTool>> {
        self.tools
            .values()
            .filter(|tool| {
                tool.spec().supported_modes.contains(&mode)
            })
            .collect()
    }

    /// Get tool definitions for all tools
    pub fn get_tool_definitions(&self) -> Vec<AnnotatedToolDefinition> {
        self.tools
            .values()
            .map(|tool| AnnotatedToolDefinition {
                name: tool.spec().name.to_string(),
                description: tool.spec().description.to_string(),
                parameters: tool.spec().parameters_schema.clone(),
                annotations: tool.spec().annotations.clone(),
            })
            .collect()
    }

    /// Get tool definitions for a specific mode
    pub fn get_tool_definitions_for_mode(&self, mode: ToolMode) -> Vec<AnnotatedToolDefinition> {
        self.tools
            .values()
            .filter(|tool| tool.spec().supported_modes.contains(&mode))
            .map(|tool| AnnotatedToolDefinition {
                name: tool.spec().name.to_string(),
                description: tool.spec().description.to_string(),
                parameters: tool.spec().parameters_schema.clone(),
                annotations: tool.spec().annotations.clone(),
            })
            .collect()
    }

    /// Register all default tools in the system
    /// This will be expanded as we implement more tools
    fn register_default_tools(&mut self) {
        // Import all tools
        use crate::tools::impls::{DeleteFilesTool, ExecuteCommandTool, ListFilesTool, ListProjectsTool, ReadFilesTool, ReplaceInFileTool, SearchFilesTool, WriteFileTool};

        // Register tools
        self.register(Box::new(DeleteFilesTool));
        self.register(Box::new(ExecuteCommandTool));
        self.register(Box::new(ListFilesTool));
        self.register(Box::new(ListProjectsTool));
        self.register(Box::new(ReadFilesTool));
        self.register(Box::new(ReplaceInFileTool));
        self.register(Box::new(SearchFilesTool));
        self.register(Box::new(WriteFileTool));

        // More tools will be added here as they are implemented
    }
}
