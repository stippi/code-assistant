use crate::tools::core::{Render, ResourcesTracker, ToolResult};
use llm::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Enhanced version of the base ToolDefinition with additional metadata fields
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotatedToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<serde_json::Value>,
}

impl AnnotatedToolDefinition {
    /// Convert to a basic ToolDefinition (without annotations) for LLM providers
    pub fn to_tool_definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        }
    }

    /// Convert a vector of AnnotatedToolDefinition to a vector of ToolDefinition
    pub fn to_tool_definitions(tools: Vec<AnnotatedToolDefinition>) -> Vec<ToolDefinition> {
        tools.into_iter().map(|t| t.to_tool_definition()).collect()
    }
}

/// Represents a tool request from the LLM, derived from ContentBlock::ToolUse
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolRequest {
    pub id: String,
    pub name: String,
    pub input: Value,
    /// Start position of the tool block in the original text (for custom syntaxes)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_offset: Option<usize>,
    /// End position of the tool block in the original text (for custom syntaxes)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_offset: Option<usize>,
}

impl From<&llm::ContentBlock> for ToolRequest {
    fn from(block: &llm::ContentBlock) -> Self {
        if let llm::ContentBlock::ToolUse { id, name, input } = block {
            Self {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
                start_offset: None,
                end_offset: None,
            }
        } else {
            panic!("Cannot convert non-ToolUse ContentBlock to ToolRequest")
        }
    }
}

/// Represents a parse error that occurred when processing tool blocks
/// This allows parse errors to be treated like regular tool results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParseError {
    pub error_message: String,
}

impl ParseError {
    pub fn new(error_message: String) -> Self {
        Self { error_message }
    }
}

impl Render for ParseError {
    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        self.error_message.clone()
    }

    fn status(&self) -> String {
        "Parse Error".to_string()
    }
}

impl ToolResult for ParseError {
    fn is_success(&self) -> bool {
        false
    }
}
