use llm::ToolDefinition;
use serde::{Deserialize, Serialize};

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
