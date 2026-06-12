//! The loop's tool-call vocabulary: abstract tool requests, executed-tool
//! records, and the placeholder outputs the loop itself produces (parse
//! errors, prompt-too-long replacements).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tools_core::{
    AnnotatedToolDefinition, AnyOutput, Render, ResourcesTracker, ToolRegistry, ToolResult,
};
use tracing::debug;

/// Convert a basic ToolDefinition (without annotations) for LLM providers
pub fn to_tool_definition(tool: &AnnotatedToolDefinition) -> llm::ToolDefinition {
    llm::ToolDefinition {
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters: tool.parameters.clone(),
    }
}

/// Convert a vector of AnnotatedToolDefinition to a vector of ToolDefinition
pub fn to_tool_definitions(tools: Vec<AnnotatedToolDefinition>) -> Vec<llm::ToolDefinition> {
    tools.iter().map(to_tool_definition).collect()
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

/// Record of a tool execution with its result
pub struct ToolExecution {
    pub tool_request: ToolRequest,
    pub result: Box<dyn AnyOutput>,
}

impl std::fmt::Debug for ToolExecution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolExecution")
            .field("tool_request", &self.tool_request)
            .field("result_success", &self.result.is_success())
            .finish()
    }
}

impl Clone for ToolExecution {
    fn clone(&self) -> Self {
        // We can't clone the actual result, but we can serialize and deserialize it.
        // Fallback to a placeholder on failure to avoid panicking — this clone is
        // called from save_state() inside the agent loop and a panic here would
        // silently kill the agent session.
        match self.try_clone() {
            Ok(cloned) => cloned,
            Err(e) => {
                tracing::error!(
                    "Failed to clone ToolExecution for tool '{}' (id={}): {}. \
                     Using placeholder to avoid panic.",
                    self.tool_request.name,
                    self.tool_request.id,
                    e
                );
                // Return a placeholder that preserves the tool request but marks
                // the result as an error so the LLM sees something sensible.
                Self::create_parse_error(
                    self.tool_request.id.clone(),
                    format!("Internal error: failed to round-trip tool result: {}", e),
                )
            }
        }
    }
}

impl ToolExecution {
    /// Attempt to clone via serialize/deserialize round-trip.
    pub fn try_clone(&self) -> Result<Self> {
        Ok(Self {
            tool_request: self.tool_request.clone(),
            result: self.result.try_clone()?,
        })
    }

    /// Create a ToolExecution for a parse error
    pub fn create_parse_error(tool_id: String, error_message: String) -> Self {
        let parse_error = ParseError::new(error_message);
        let tool_request = ToolRequest {
            id: tool_id,
            name: "parse_error".to_string(),
            input: Value::Null,
            start_offset: None,
            end_offset: None,
        };

        Self {
            tool_request,
            result: Box::new(parse_error),
        }
    }

    /// Serialize the tool execution to a storable format
    pub fn serialize(&self) -> Result<SerializedToolExecution> {
        // Try to serialize the result, but fallback to a simple representation if it fails
        let result_json = match self.result.to_json() {
            Ok(json) => json,
            Err(e) => {
                debug!("Failed to serialize tool result, using fallback: {}", e);
                serde_json::json!({
                    "error": "Failed to serialize result",
                    "success": self.result.is_success(),
                    "details": format!("{}", e)
                })
            }
        };

        Ok(SerializedToolExecution {
            tool_request: self.tool_request.clone(),
            result_json,
            tool_name: self.tool_request.name.clone(),
        })
    }
}

/// Serialized representation of a tool execution
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SerializedToolExecution {
    /// Tool request details
    pub tool_request: ToolRequest,
    /// Serialized tool result as JSON
    pub result_json: serde_json::Value,
    /// Tool name for deserialization
    pub tool_name: String,
}

impl SerializedToolExecution {
    /// Deserialize back to a ToolExecution
    pub fn deserialize(&self, registry: &ToolRegistry) -> Result<ToolExecution> {
        // Special handling for parse errors
        if self.tool_name == "parse_error" {
            let parse_error: ParseError = serde_json::from_value(self.result_json.clone())?;
            let result: Box<dyn AnyOutput> = Box::new(parse_error);

            return Ok(ToolExecution {
                tool_request: self.tool_request.clone(),
                result,
            });
        }

        let tool = registry
            .get(&self.tool_name)
            .ok_or_else(|| anyhow::anyhow!("Tool not found: {}", self.tool_name))?;

        let result = tool.deserialize_output(self.result_json.clone())?;

        Ok(ToolExecution {
            tool_request: self.tool_request.clone(),
            result,
        })
    }
}

/// Generate a text summary from content blocks for UI display.
/// Images are shown as `[image/png]` etc., text blocks are joined with newlines.
pub fn text_summary_from_blocks(blocks: &[llm::ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            llm::ContentBlock::Text { text, .. } => Some(text.clone()),
            llm::ContentBlock::Image { media_type, .. } => Some(format!("[{media_type}]")),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
