use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// Capability tags describing what a tool may do and where it is offered.
/// Free-form strings; the constants below cover generic concepts. Consumers
/// define their own additional tags, e.g. scoping vocabulary saying where a
/// tool is offered.
pub mod capabilities {
    /// The tool does not modify any state. Read-only tools are safe to chain
    /// within a single assistant turn.
    pub const READ_ONLY: &str = "read_only";
    /// The tool modifies files in a project.
    pub const EDITS_FILES: &str = "edits_files";
}

/// Specification for a tool, including metadata
///
/// Name, description and capabilities are `Cow`s so that built-in tools can
/// use plain literals while tools discovered at runtime (e.g. offered by an
/// MCP server) carry owned strings.
#[derive(Clone)]
pub struct ToolSpec {
    /// Unique name of the tool
    pub name: Cow<'static, str>,
    /// Detailed description of what the tool does
    pub description: Cow<'static, str>,
    /// JSON Schema for the tool's parameters
    pub parameters_schema: serde_json::Value,
    /// Optional annotations for LLM-specific instructions
    pub annotations: Option<serde_json::Value>,
    /// Capability tags (see [`capabilities`]) consumers select tools by,
    /// including the scope tags saying where the tool is offered
    pub capabilities: Vec<Cow<'static, str>>,
    /// Parameters whose values typically span multiple lines. Text dialects
    /// (XML, Caret) render these with block syntax; native tool calling
    /// ignores this.
    pub multiline_params: &'static [&'static str],
    /// Whether this tool should be hidden from UI display
    pub hidden: bool,
    /// Optional template for generating dynamic titles from parameters
    /// Use {parameter_name} placeholders, e.g. "Reading {paths}" or "Searching for '{regex}'"
    pub title_template: Option<&'static str>,
}

impl ToolSpec {
    /// Convert a list of `&'static str` tags into the owned-capable
    /// capabilities vector. Convenience for the common literal case:
    /// `capabilities: ToolSpec::capabilities(&[capabilities::READ_ONLY])`.
    pub fn capabilities(tags: &[&'static str]) -> Vec<Cow<'static, str>> {
        tags.iter().map(|tag| Cow::Borrowed(*tag)).collect()
    }

    pub fn has_capability(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|tag| tag == capability)
    }

    pub fn is_multiline_param(&self, name: &str) -> bool {
        self.multiline_params.contains(&name)
    }
}

/// A tool definition as offered to an LLM: the owned counterpart of
/// [`ToolSpec`], extended with optional annotations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotatedToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_spec_supports_runtime_owned_strings() {
        // Tools discovered at runtime (e.g. from an MCP server) only know
        // their name, description and capability tags as owned strings.
        let server = String::from("jira");
        let scope_tag = format!("scope:mcp-{server}");
        let spec = ToolSpec {
            name: format!("mcp__{server}__search_issues").into(),
            description: String::from("Discovered at runtime").into(),
            parameters_schema: serde_json::json!({"type": "object"}),
            annotations: None,
            capabilities: vec![capabilities::READ_ONLY.into(), scope_tag.into()],
            multiline_params: &[],
            hidden: false,
            title_template: None,
        };
        assert_eq!(spec.name, "mcp__jira__search_issues");
        assert!(spec.has_capability("scope:mcp-jira"));
        assert!(spec.has_capability(capabilities::READ_ONLY));
        assert!(!spec.has_capability(capabilities::EDITS_FILES));
    }

    #[test]
    fn tool_spec_supports_static_literals() {
        let spec = ToolSpec {
            name: "read_files".into(),
            description: "Reads files".into(),
            parameters_schema: serde_json::json!({"type": "object"}),
            annotations: None,
            capabilities: vec![capabilities::READ_ONLY.into()],
            multiline_params: &["content"],
            hidden: false,
            title_template: Some("Reading {paths}"),
        };
        assert!(spec.has_capability("read_only"));
        assert!(spec.is_multiline_param("content"));
    }
}
