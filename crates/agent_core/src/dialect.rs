//! How a tool call travels between the LLM and the agent (§3.7 of the
//! extraction plan).
//!
//! The agent loop knows only this abstraction: it extracts abstract
//! [`ToolRequest`]s from responses, formats a request back into text for the
//! format-on-save path, and obtains a stream processor for the UI. Which
//! concrete syntax is spoken — native tool calling, XML, or Caret — is
//! decided once at agent construction by picking an implementation. This
//! crate ships exactly one: [`crate::native::NativeDialect`]; text dialects
//! are application code.

use crate::types::ToolRequest;
use crate::ui::{AgentUi, HiddenTools, StreamProcessorTrait};
use anyhow::Result;
use llm::{LLMResponse, Message};
use std::sync::Arc;
use tools_core::ToolRegistry;

pub trait ToolDialect: Send + Sync {
    /// Extract `ToolRequest`s from a completed LLM response and return a
    /// variant of the response truncated after the first tool block, so
    /// trailing text does not end up in the transcript. `order_offset`
    /// continues counting tools already extracted for this request, keeping
    /// generated tool IDs unique.
    fn extract_requests(
        &self,
        response: &LLMResponse,
        request_id: u64,
        order_offset: usize,
        registry: &ToolRegistry,
    ) -> Result<(Vec<ToolRequest>, LLMResponse)>;

    /// Format a `ToolRequest` back into this dialect's text representation.
    /// Used by the format-on-save path when a tool rewrites its input during
    /// execution and the originating call must be replaced in the message
    /// history.
    fn format_tool_request(
        &self,
        request: &ToolRequest,
        registry: &ToolRegistry,
    ) -> Result<String>;

    /// Whether tool calls and results travel through the LLM API natively
    /// (`ToolUse`/`ToolResult` blocks and the request's `tools` field). Text
    /// dialects return `false`: their tool results are rendered to text
    /// before the request and the tools are described in the system prompt.
    fn uses_native_tools(&self) -> bool;

    /// A stream processor that translates `StreamingChunk`s into display
    /// fragments for the UI. `hidden_tools` decides which tool invocations
    /// are suppressed; `registry` lets text dialects consult tool metadata
    /// (e.g. capability-based chaining rules) while streaming.
    fn stream_processor(
        &self,
        ui: Arc<dyn AgentUi>,
        request_id: u64,
        hidden_tools: HiddenTools,
        registry: Arc<ToolRegistry>,
    ) -> Box<dyn StreamProcessorTrait>;

    /// Format description for the system prompt ("this is how you call
    /// tools…"). `None` for native tool calling.
    fn render_format_section_for_prompt(&self) -> Option<String>;

    /// Tool documentation block for the system prompt, covering the tools in
    /// `registry` that carry `capability`. `None` for native tool calling.
    fn render_tool_section_for_prompt(
        &self,
        registry: &ToolRegistry,
        capability: &str,
    ) -> Option<String>;

    /// Whether an already stored message contains a tool invocation in this
    /// dialect (used to normalize the history when loading a session).
    fn message_contains_invocation(&self, message: &Message, registry: &ToolRegistry) -> bool;
}
