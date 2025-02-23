use crate::llm::{
    streaming::{process_sse_data, ContentAccumulator, LineBuffer, StreamResponseHandler},
    types::*,
    StreamingCallback,
};
use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct StreamEventCommon {
    #[allow(dead_code)]
    index: usize,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum StreamEvent {
    #[allow(dead_code)]
    #[serde(rename = "message_start")]
    MessageStart { message: MessageStart },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        #[serde(flatten)]
        #[allow(dead_code)]
        common: StreamEventCommon,
        content_block: StreamContentBlock,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta {
        #[serde(flatten)]
        #[allow(dead_code)]
        common: StreamEventCommon,
        delta: ContentDelta,
    },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop {
        #[serde(flatten)]
        #[allow(dead_code)]
        common: StreamEventCommon,
    },
    #[serde(rename = "message_delta")]
    MessageDelta,
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
}

#[derive(Debug, Deserialize)]
struct MessageStart {
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    #[serde(rename = "type")]
    message_type: String,
    #[allow(dead_code)]
    role: String,
    #[allow(dead_code)]
    model: String,
}

#[derive(Debug, Deserialize)]
struct StreamContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: Option<String>,
    // Fields for tool use blocks
    id: Option<String>,
    name: Option<String>,
    #[allow(dead_code)]
    input: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ContentDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
}

#[derive(Clone)]
pub struct AnthropicStreamHandler {
    line_buffer: LineBuffer,
    content_accumulator: ContentAccumulator,
    tool_calls: Vec<ContentBlock>,
    current_tool_call: Option<(String, String, String)>, // (id, name, input)
}

impl AnthropicStreamHandler {
    pub fn new() -> Self {
        Self {
            line_buffer: LineBuffer::new(),
            content_accumulator: ContentAccumulator::new(),
            tool_calls: Vec::new(),
            current_tool_call: None,
        }
    }

    fn process_line(&mut self, line: &str, callback: &StreamingCallback) -> Result<bool> {
        if let Some(event) =
            process_sse_data(line, |data| Ok(serde_json::from_str::<StreamEvent>(data)?))?
        {
            match event {
                StreamEvent::ContentBlockStart { content_block, .. } => {
                    match content_block.block_type.as_str() {
                        "text" => {
                            if let Some(text) = content_block.text {
                                if !text.is_empty() {
                                    callback(&text)?;
                                    self.content_accumulator.append(&text)?;
                                }
                            }
                        }
                        "tool_use" => {
                            if let (Some(id), Some(name)) = (content_block.id, content_block.name) {
                                self.current_tool_call = Some((id, name, String::new()));
                            }
                        }
                        _ => {}
                    }
                }
                StreamEvent::ContentBlockDelta { delta, .. } => match delta {
                    ContentDelta::TextDelta { text } => {
                        callback(&text)?;
                        self.content_accumulator.append(&text)?;
                    }
                    ContentDelta::InputJsonDelta { partial_json } => {
                        if let Some((_, _, ref mut input)) = self.current_tool_call {
                            input.push_str(&partial_json);
                        }
                    }
                },
                StreamEvent::ContentBlockStop { .. } => {
                    if let Some((id, name, input)) = self.current_tool_call.take() {
                        if let Ok(json) = serde_json::from_str(&input) {
                            self.tool_calls.push(ContentBlock::ToolUse {
                                id,
                                name,
                                input: json,
                            });
                        }
                    }
                }
                StreamEvent::MessageStop => {
                    return Ok(true);
                }
                _ => {}
            }
        }

        Ok(false)
    }
}

impl AnthropicStreamHandler {
    fn handle_line(&mut self, line: &str, callback: &StreamingCallback) -> Result<bool> {
        let mut done = false;
        if let Ok(is_done) = self.process_line(line, callback) {
            done = is_done;
        }
        Ok(done)
    }
}

impl StreamResponseHandler for AnthropicStreamHandler {
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
                input_tokens: 0,
                output_tokens: 0,
            },
        })
    }
}
