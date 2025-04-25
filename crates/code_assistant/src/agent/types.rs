use crate::tools::core::AnyOutput;
use serde_json::Value;

/// Represents a tool request from the LLM, derived from ContentBlock::ToolUse
#[derive(Debug, Clone)]
pub struct ToolRequest {
    pub id: String,
    pub name: String,
    pub input: Value,
}

impl From<&llm::ContentBlock> for ToolRequest {
    fn from(block: &llm::ContentBlock) -> Self {
        if let llm::ContentBlock::ToolUse { id, name, input } = block {
            Self {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            }
        } else {
            panic!("Cannot convert non-ToolUse ContentBlock to ToolRequest")
        }
    }
}

/// Record of a tool execution with its result
pub struct ToolExecution {
    pub tool_request: ToolRequest,
    pub result: Box<dyn AnyOutput>,
}
