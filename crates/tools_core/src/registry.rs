use std::collections::HashMap;

use crate::dyn_tool::DynTool;
use crate::spec::AnnotatedToolDefinition;

/// Registry holding a set of tools, keyed by name.
///
/// Selection is expressed through capability tags (see
/// [`crate::spec::ToolSpec::capabilities`]); the registry itself knows
/// nothing about application-specific scopes.
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn DynTool>>,
}

impl ToolRegistry {
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

    /// A shareable predicate over [`Self::is_tool_hidden`] for the given
    /// capability tag. Lets UI-layer consumers check hidden-ness without
    /// referencing the registry.
    pub fn hidden_tools(
        self: &std::sync::Arc<Self>,
        capability: &str,
    ) -> std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync> {
        let registry = self.clone();
        let capability = capability.to_string();
        std::sync::Arc::new(move |name| registry.is_tool_hidden(name, &capability))
    }

    /// Check if a tool carrying the given capability tag is hidden
    pub fn is_tool_hidden(&self, tool_name: &str, capability: &str) -> bool {
        self.tools
            .values()
            .filter(|tool| tool.spec().has_capability(capability))
            .find(|tool| tool.spec().name == tool_name)
            .map(|tool| tool.spec().hidden)
            .unwrap_or(false)
    }

    /// Get the definitions of all tools carrying the given capability tag
    pub fn get_tool_definitions_with_capability(
        &self,
        capability: &str,
    ) -> Vec<AnnotatedToolDefinition> {
        self.tools
            .values()
            .filter(|tool| tool.spec().has_capability(capability))
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
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
