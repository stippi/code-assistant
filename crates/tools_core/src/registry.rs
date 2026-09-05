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

    /// Like [`Self::get_tool_definitions_with_capability`], but omits tools
    /// that also carry any capability tag in `excluded`. Used to hide specific
    /// MCP servers (each tool tagged `scope:mcp-<server>`) from a session
    /// without mutating the shared registry.
    pub fn get_tool_definitions_with_capability_excluding(
        &self,
        capability: &str,
        excluded: &[String],
    ) -> Vec<AnnotatedToolDefinition> {
        self.tools
            .values()
            .filter(|tool| tool.spec().has_capability(capability))
            .filter(|tool| !excluded.iter().any(|cap| tool.spec().has_capability(cap)))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dyn_tool::{AnyOutput, DynTool};
    use crate::spec::ToolSpec;
    use crate::tool::ToolContext;
    use async_trait::async_trait;
    use serde_json::Value;

    struct FakeTool {
        name: &'static str,
        tags: &'static [&'static str],
    }

    #[async_trait]
    impl DynTool for FakeTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.into(),
                description: "test".into(),
                parameters_schema: serde_json::json!({"type": "object"}),
                annotations: None,
                capabilities: ToolSpec::capabilities(self.tags),
                multiline_params: &[],
                hidden: false,
                title_template: None,
            }
        }
        async fn invoke<'a>(
            &self,
            _: &mut ToolContext<'a>,
            _: &mut Value,
        ) -> anyhow::Result<Box<dyn AnyOutput>> {
            unreachable!("not exercised in these tests")
        }
        fn deserialize_output(&self, _: Value) -> anyhow::Result<Box<dyn AnyOutput>> {
            unreachable!("not exercised in these tests")
        }
    }

    fn tool(name: &'static str, tags: &'static [&'static str]) -> Box<dyn DynTool> {
        Box::new(FakeTool { name, tags })
    }

    #[test]
    fn excluding_drops_tools_carrying_an_excluded_tag() {
        let mut registry = ToolRegistry::new();
        registry.register(tool("mcp__a__x", &["scope:agent", "scope:mcp-a"]));
        registry.register(tool("mcp__b__y", &["scope:agent", "scope:mcp-b"]));
        registry.register(tool("edit", &["scope:agent"]));

        assert_eq!(
            registry
                .get_tool_definitions_with_capability("scope:agent")
                .len(),
            3
        );

        let filtered = registry
            .get_tool_definitions_with_capability_excluding("scope:agent", &["scope:mcp-a".into()]);
        let names: Vec<&str> = filtered.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"mcp__b__y"));
        assert!(names.contains(&"edit"));
        assert!(
            !names.contains(&"mcp__a__x"),
            "the excluded server's tool must be dropped"
        );
    }

    #[test]
    fn excluding_empty_matches_plain_and_respects_capability() {
        let mut registry = ToolRegistry::new();
        registry.register(tool("edit", &["scope:agent"]));
        registry.register(tool("peek", &["scope:subagent"]));

        let filtered = registry.get_tool_definitions_with_capability_excluding("scope:agent", &[]);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "edit");
    }
}
