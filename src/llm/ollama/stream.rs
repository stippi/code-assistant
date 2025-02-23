use crate::llm::{
    streaming::{ContentAccumulator, LineBuffer, StreamResponseHandler},
    types::*,
    StreamingCallback,
};
use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct OllamaResponse {
    message: OllamaResponseMessage,
    #[allow(dead_code)]
    done_reason: Option<String>,
    done: bool,
    #[serde(default)]
    prompt_eval_count: u32,
    #[serde(default)]
    eval_count: u32,
}

#[derive(Debug, Deserialize)]
struct OllamaResponseMessage {
    content: String,
    tool_calls: Option<Vec<OllamaToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OllamaToolCall {
    function: OllamaFunction,
}

#[derive(Debug, Deserialize)]
struct OllamaFunction {
    name: String,
    arguments: serde_json::Value,
}

#[derive(Clone)]
pub struct OllamaStreamHandler {
    line_buffer: LineBuffer,
    content_accumulator: ContentAccumulator,
    tool_calls: Vec<ContentBlock>,
    eval_counts: (u32, u32), // (prompt_eval_count, eval_count)
}

impl OllamaStreamHandler {
    pub fn new() -> Self {
        Self {
            line_buffer: LineBuffer::new(),
            content_accumulator: ContentAccumulator::new(),
            tool_calls: Vec::new(),
            eval_counts: (0, 0),
        }
    }

    fn process_line(&mut self, line: &str, callback: &StreamingCallback) -> Result<bool> {
        if let Ok(response) = serde_json::from_str::<OllamaResponse>(line) {
            // Handle text content
            if !response.message.content.is_empty() {
                callback(&response.message.content)?;
                self.content_accumulator.append(&response.message.content)?;
            }

            // Handle tool calls
            if let Some(tool_calls) = response.message.tool_calls {
                for (index, tool_call) in tool_calls.into_iter().enumerate() {
                    self.tool_calls.push(ContentBlock::ToolUse {
                        id: format!("tool-{}", index),
                        name: tool_call.function.name,
                        input: tool_call.function.arguments,
                    });
                }
            }

            // Update eval counts from the response
            self.eval_counts = (response.prompt_eval_count, response.eval_count);

            // Return done status
            return Ok(response.done);
        }

        Ok(false)
    }
}

impl OllamaStreamHandler {
    fn handle_line(&mut self, line: &str, callback: &StreamingCallback) -> Result<bool> {
        let mut done = false;
        if let Ok(is_done) = self.process_line(line, callback) {
            done = is_done;
        }
        Ok(done)
    }
}

impl StreamResponseHandler for OllamaStreamHandler {
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
            usage: Usage {
                input_tokens: self.eval_counts.0,
                output_tokens: self.eval_counts.1,
            },
        })
    }
}
