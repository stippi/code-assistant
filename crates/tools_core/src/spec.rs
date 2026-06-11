use serde::{Deserialize, Serialize};

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
#[derive(Clone)]
pub struct ToolSpec {
    /// Unique name of the tool
    pub name: &'static str,
    /// Detailed description of what the tool does
    pub description: &'static str,
    /// JSON Schema for the tool's parameters
    pub parameters_schema: serde_json::Value,
    /// Optional annotations for LLM-specific instructions
    pub annotations: Option<serde_json::Value>,
    /// Capability tags (see [`capabilities`]) consumers select tools by,
    /// including the scope tags saying where the tool is offered
    pub capabilities: &'static [&'static str],
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
    pub fn has_capability(&self, capability: &str) -> bool {
        self.capabilities.contains(&capability)
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
