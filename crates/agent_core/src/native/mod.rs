//! The built-in default [`ToolDialect`]: native tool calling via the LLM
//! API. Tool calls arrive as `ToolUse` blocks, tool results travel back as
//! `ToolResult` blocks, and the tool list goes into the request's `tools`
//! field — no text syntax involved.

mod json_processor;

pub use json_processor::JsonStreamProcessor;

use crate::dialect::ToolDialect;
use crate::types::ToolRequest;
use crate::ui::{AgentUi, HiddenTools, StreamProcessorTrait};
use anyhow::Result;
use llm::{ContentBlock, LLMResponse, Message, MessageContent};
use std::sync::Arc;
use tools_core::ToolRegistry;

pub struct NativeDialect;

impl ToolDialect for NativeDialect {
    fn extract_requests(
        &self,
        response: &LLMResponse,
        _request_id: u64,
        _order_offset: usize,
    ) -> Result<(Vec<ToolRequest>, LLMResponse)> {
        let mut tool_requests = Vec::new();

        for block in &response.content {
            if let ContentBlock::ToolUse {
                id, name, input, ..
            } = block
            {
                tool_requests.push(ToolRequest {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                    start_offset: None,
                    end_offset: None,
                });
            }
        }

        Ok((tool_requests, response.clone()))
    }

    fn format_tool_request(
        &self,
        request: &ToolRequest,
        _registry: &ToolRegistry,
    ) -> Result<String> {
        // Native tools are represented as JSON function calls
        // Return the input serialized as JSON string
        Ok(serde_json::to_string(&request.input)?)
    }

    fn uses_native_tools(&self) -> bool {
        true
    }

    fn stream_processor(
        &self,
        ui: Arc<dyn AgentUi>,
        request_id: u64,
        hidden_tools: HiddenTools,
    ) -> Box<dyn StreamProcessorTrait> {
        Box::new(JsonStreamProcessor::new(ui, request_id, hidden_tools))
    }

    fn render_format_section_for_prompt(&self) -> Option<String> {
        // Native mode uses API-provided function calls, no custom syntax documentation needed
        None
    }

    fn render_tool_section_for_prompt(
        &self,
        _registry: &ToolRegistry,
        _capability: &str,
    ) -> Option<String> {
        // Native mode uses API-provided tool definitions, no custom documentation needed
        None
    }

    fn message_contains_invocation(&self, message: &Message) -> bool {
        if let MessageContent::Structured(blocks) = &message.content {
            blocks
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolUse { .. }))
        } else {
            false
        }
    }
}
