use crate::llm::{
    streaming::{process_sse_data, ContentAccumulator, LineBuffer, StreamResponseHandler},
    types::*,
    StreamingCallback,
};
use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct OpenAIStreamResponse {
    choices: Vec<OpenAIStreamChoice>,
    #[serde(default)]
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamChoice {
    delta: OpenAIDelta,
    #[serde(rename = "finish_reason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIDelta {
    #[serde(default)]
    content: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAIToolCallDelta>>,
}

#[derive(Debug, Deserialize, Clone)]
struct OpenAIToolCallDelta {
    #[allow(dead_code)]
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[allow(dead_code)]
    #[serde(rename = "type")]
    #[serde(default)]
    call_type: Option<String>,
    #[serde(default)]
    function: Option<OpenAIFunctionDelta>,
}

use super::types::OpenAIUsage;

#[derive(Debug, Deserialize, Clone)]
struct OpenAIFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Clone)]
pub struct OpenAIStreamHandler {
    line_buffer: LineBuffer,
    content_accumulator: ContentAccumulator,
    tool_calls: Vec<ContentBlock>,
    current_tool: Option<OpenAIToolCallDelta>,
    usage: Option<OpenAIUsage>,
}

impl OpenAIStreamHandler {
    pub fn new() -> Self {
        Self {
            line_buffer: LineBuffer::new(),
            content_accumulator: ContentAccumulator::new(),
            tool_calls: Vec::new(),
            current_tool: None,
            usage: None,
        }
    }

    fn process_line(&mut self, line: &str, callback: &StreamingCallback) -> Result<bool> {
        if let Some(chunk_response) = process_sse_data(line, |data| {
            Ok(serde_json::from_str::<OpenAIStreamResponse>(data)?)
        })? {
            if let Some(delta) = chunk_response.choices.get(0) {
                // Handle content streaming
                if let Some(content) = &delta.delta.content {
                    callback(content)?;
                    self.content_accumulator.append(content)?;
                }

                // Handle tool calls
                if let Some(tool_calls) = &delta.delta.tool_calls {
                    for tool_call in tool_calls {
                        if let Some(function) = &tool_call.function {
                            if tool_call.id.is_some() {
                                // New tool call
                                if let Some(prev_tool) = self.current_tool.take() {
                                    self.tool_calls.push(Self::build_tool_block(prev_tool)?);
                                }
                                self.current_tool = Some(tool_call.clone());
                            } else if let Some(curr_tool) = &mut self.current_tool {
                                // Update existing tool
                                if let Some(args) = &function.arguments {
                                    if let Some(ref mut curr_func) = curr_tool.function {
                                        curr_func.arguments = Some(
                                            curr_func
                                                .arguments
                                                .as_ref()
                                                .unwrap_or(&String::new())
                                                .clone()
                                                + args,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }

                // Handle completion
                if delta.finish_reason.is_some() {
                    if let Some(tool) = self.current_tool.take() {
                        self.tool_calls.push(Self::build_tool_block(tool)?);
                    }
                    return Ok(true);
                }
            }

            // Capture usage data from final chunk
            if let Some(chunk_usage) = chunk_response.usage {
                self.usage = Some(chunk_usage);
            }
        }

        Ok(false)
    }

    fn build_tool_block(tool: OpenAIToolCallDelta) -> Result<ContentBlock> {
        let function = tool
            .function
            .ok_or_else(|| anyhow::anyhow!("Tool call without function"))?;
        let name = function
            .name
            .ok_or_else(|| anyhow::anyhow!("Function without name"))?;
        let args = function.arguments.unwrap_or_default();

        Ok(ContentBlock::ToolUse {
            id: tool.id.unwrap_or_default(),
            name,
            input: serde_json::from_str(&args)
                .map_err(|e| anyhow::anyhow!("Invalid JSON in arguments: {}", e))?,
        })
    }
}

impl OpenAIStreamHandler {
    fn handle_line(&mut self, line: &str, callback: &StreamingCallback) -> Result<bool> {
        let mut done = false;
        if let Ok(is_done) = self.process_line(line, callback) {
            done = is_done;
        }
        Ok(done)
    }
}

impl StreamResponseHandler for OpenAIStreamHandler {
    fn process_chunk(&mut self, chunk: &[u8], callback: &StreamingCallback) -> Result<bool> {
        let mut done = false;
        let mut this = self.clone();
        self.line_buffer.process_chunk(chunk, |line| {
            if let Ok(is_done) = this.handle_line(line, callback) {
                done = is_done;
            }
            Ok(())
        })?;
        *self = this;
        Ok(done)
    }

    fn into_response(mut self) -> Result<LLMResponse> {
        let noop_callback: StreamingCallback = Box::new(|_| Ok(()));
        // Process any remaining data
        self.line_buffer.flush(&noop_callback)?;

        let mut content = Vec::new();

        // Add accumulated text content if present
        if let Some(text_block) = self.content_accumulator.into_content_block() {
            content.push(text_block);
        }

        // Add tool calls
        content.extend(self.tool_calls);

        Ok(LLMResponse {
            content,
            usage: self
                .usage
                .map(|u| Usage {
                    input_tokens: u.prompt_tokens,
                    output_tokens: u.completion_tokens,
                })
                .unwrap_or(Usage {
                    input_tokens: 0,
                    output_tokens: 0,
                }),
        })
    }
}
