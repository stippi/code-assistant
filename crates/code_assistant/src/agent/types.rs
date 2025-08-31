use crate::tools::core::{AnyOutput, ToolRegistry};
use crate::tools::ToolRequest;
use anyhow::Result;
use tracing::debug;

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
        // We can't clone the actual result, but we can serialize and deserialize it
        let serialized = self
            .serialize()
            .expect("Failed to serialize tool execution for cloning");
        serialized
            .deserialize()
            .expect("Failed to deserialize tool execution for cloning")
    }
}

impl ToolExecution {
    /// Create a ToolExecution for a parse error
    pub fn create_parse_error(tool_id: String, error_message: String) -> Self {
        use crate::tools::{ParseError, ToolRequest};
        use serde_json::Value;

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
    pub fn serialize(&self) -> Result<crate::persistence::SerializedToolExecution> {
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

        Ok(crate::persistence::SerializedToolExecution {
            tool_request: self.tool_request.clone(),
            result_json,
            tool_name: self.tool_request.name.clone(),
        })
    }
}

impl crate::persistence::SerializedToolExecution {
    /// Deserialize back to a ToolExecution
    pub fn deserialize(&self) -> Result<ToolExecution> {
        // Special handling for parse errors
        if self.tool_name == "parse_error" {
            use crate::tools::ParseError;

            let parse_error: ParseError = serde_json::from_value(self.result_json.clone())?;
            let result: Box<dyn AnyOutput> = Box::new(parse_error);

            return Ok(ToolExecution {
                tool_request: self.tool_request.clone(),
                result,
            });
        }

        let tool = ToolRegistry::global()
            .get(&self.tool_name)
            .ok_or_else(|| anyhow::anyhow!("Tool not found: {}", self.tool_name))?;

        let result = tool.deserialize_output(self.result_json.clone())?;

        Ok(ToolExecution {
            tool_request: self.tool_request.clone(),
            result,
        })
    }
}
