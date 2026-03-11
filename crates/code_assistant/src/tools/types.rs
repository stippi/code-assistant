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
        if let llm::ContentBlock::ToolUse {
            id, name, input, ..
        } = block
        {
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

/// Placeholder result used when a tool's output was too large and caused a
/// "prompt too long" error from the LLM provider.  The original tool execution
/// is replaced with this so the LLM gets actionable feedback on the next turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptTooLongError {
    pub error_message: String,
}

impl PromptTooLongError {
    pub fn new(tool_name: &str, output_size_bytes: usize) -> Self {
        let size_kb = output_size_bytes / 1024;
        Self {
            error_message: format!(
                "Tool result omitted — the total prompt exceeded the model's context limit. \
                 The output of '{tool_name}' was approximately {size_kb}KB. \
                 Consider more targeted approaches:\n\
                 - For read_files: use line ranges (e.g. file.txt:1-200) or search_files to find relevant sections\n\
                 - For execute_command: pipe output through head/tail/grep to limit size\n\
                 - For web_fetch: use CSS selectors to extract specific content"
            ),
        }
    }
}

impl Render for PromptTooLongError {
    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        self.error_message.clone()
    }

    fn status(&self) -> String {
        "Prompt Too Long".to_string()
    }
}

impl ToolResult for PromptTooLongError {
    fn is_success(&self) -> bool {
        false
    }
}
