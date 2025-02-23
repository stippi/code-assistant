
use crate::llm::{
    streaming::{ContentAccumulator, LineBuffer, StreamResponseHandler, process_sse_data},
    types::*,
    StreamingCallback,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
struct VertexResponse {
    candidates: Vec<VertexCandidate>,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<VertexUsageMetadata>,
}

#[derive(Debug, Deserialize, Clone)]
struct VertexUsageMetadata {
    #[serde(rename = "promptTokenCount")]
    prompt_token_count: u32,
    #[serde(rename = "candidatesTokenCount")]
    candidates_token_count: u32,
    #[allow(dead_code)]
    #[serde(rename = "totalTokenCount")]
    total_token_count: u32,
}

#[derive(Debug, Deserialize)]
struct VertexCandidate {
    content: VertexContent,
}

#[derive(Debug, Serialize, Deserialize)]
struct VertexContent {
    parts: Vec<VertexPart>,
    role: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct VertexPart {
    #[serde(rename = "functionCall")]
    function_call: Option<VertexFunctionCall>,
    // Optional text field for text responses
    text: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct VertexFunctionCall {
    name: String,
    args: serde_json::Value,
}

#[derive(Clone)]
pub struct VertexStreamHandler {
    line_buffer: LineBuffer,
    content_accumulator: ContentAccumulator,
    tool_calls: Vec<ContentBlock>,
    last_usage: Option<VertexUsageMetadata>,
}

impl VertexStreamHandler {
    pub fn new() -> Self {
        Self {
            line_buffer: LineBuffer::new(),
            content_accumulator: ContentAccumulator::new(),
            tool_calls: Vec::new(),
            last_usage: None,
        }
    }

    fn process_line(&mut self, line: &str, callback: &StreamingCallback) -> Result<bool> {
        if let Some(response) = process_sse_data(line, |data| {
            Ok(serde_json::from_str::<VertexResponse>(data)?)
        })? {
            if let Some(candidate) = response.candidates.first() {
                for part in &candidate.content.parts {
                    if let Some(text) = &part.text {
                        callback(text)?;
                        self.content_accumulator.append(text)?;
                    } else if let Some(function_call) = &part.function_call {
                        self.tool_calls.push(ContentBlock::ToolUse {
                            id: format!("tool-{}", self.tool_calls.len()),
                            name: function_call.name.clone(),
                            input: function_call.args.clone(),
                        });
                    }
                }
            }

            // Update usage metadata if present
            if let Some(usage) = response.usage_metadata {
                self.last_usage = Some(usage);
            }
        }

        Ok(false)
    }
}

impl VertexStreamHandler {
    fn handle_line(&mut self, line: &str, callback: &StreamingCallback) -> Result<bool> {
        let mut done = false;
        if let Ok(is_done) = self.process_line(line, callback) {
            done = is_done;
        }
        Ok(done)
    }
}

impl StreamResponseHandler for VertexStreamHandler {
    fn process_chunk(&mut self, chunk: &[u8], callback: &StreamingCallback) -> Result<bool> {
        let mut done = false;
        let mut this = self.clone();
        self.line_buffer
            .process_chunk(chunk, |line| {
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
                input_tokens: self
                    .last_usage
                    .as_ref()
                    .map(|u| u.prompt_token_count)
                    .unwrap_or(0),
                output_tokens: self
                    .last_usage
                    .as_ref()
                    .map(|u| u.candidates_token_count)
                    .unwrap_or(0),
            },
        })
    }
}
