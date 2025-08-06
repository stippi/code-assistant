use crate::{types::*, LLMProvider, StreamingCallback, StreamingChunk};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

#[derive(Debug, Serialize)]
struct OllamaRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
    options: OllamaOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Serialize)]
struct OllamaOptions {
    num_ctx: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct OllamaMessage {
    #[serde(default)]
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<Vec<String>>,
    tool_calls: Option<Vec<OllamaToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OllamaResponse {
    message: OllamaMessage,
    #[allow(dead_code)]
    done_reason: Option<String>,
    done: bool,
    #[serde(default)]
    prompt_eval_count: u32,
    #[serde(default)]
    eval_count: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct OllamaToolCall {
    function: OllamaFunction,
}

#[derive(Debug, Serialize, Deserialize)]
struct OllamaFunction {
    name: String,
    arguments: serde_json::Value,
}

pub struct OllamaClient {
    client: Client,
    base_url: String,
    model: String,
    num_ctx: usize,
}

impl OllamaClient {
    pub fn default_base_url() -> String {
        "http://localhost:11434".to_string()
    }

    pub fn new(model: String, base_url: String, num_ctx: usize) -> Self {
        Self {
            client: Client::new(),
            base_url,
            model,
            num_ctx,
        }
    }

    fn get_url(&self) -> String {
        format!("{}/api/chat", self.base_url)
    }

    fn convert_message(message: &Message) -> Vec<OllamaMessage> {
        match &message.content {
            MessageContent::Text(text) => {
                vec![OllamaMessage {
                    role: match message.role {
                        MessageRole::User => "user".to_string(),
                        MessageRole::Assistant => "assistant".to_string(),
                    },
                    content: text.clone(),
                    images: None,
                    tool_calls: None,
                }]
            }
            MessageContent::Structured(blocks) => Self::convert_structured_content(message, blocks),
        }
    }

    fn convert_structured_content(
        message: &Message,
        blocks: &[ContentBlock],
    ) -> Vec<OllamaMessage> {
        match message.role {
            MessageRole::Assistant => {
                // For Assistant: Collect all ToolUse in tool_calls, rest as content
                Self::convert_assistant_message(blocks)
            }
            MessageRole::User => {
                // For User: Separate messages for ToolResult (role="tool"), rest combined
                Self::convert_user_message(blocks)
            }
        }
    }

    fn convert_assistant_message(blocks: &[ContentBlock]) -> Vec<OllamaMessage> {
        let mut content_parts = Vec::new();
        let mut tool_calls = Vec::new();
        let mut images = Vec::new();

        for block in blocks {
            match block {
                ContentBlock::Text { text } => content_parts.push(text.clone()),
                ContentBlock::Image { data, .. } => images.push(data.clone()),
                ContentBlock::ToolUse { name, input, .. } => {
                    tool_calls.push(OllamaToolCall {
                        function: OllamaFunction {
                            name: name.clone(),
                            arguments: input.clone(),
                        },
                    });
                }
                ContentBlock::Thinking { thinking, .. } => content_parts.push(thinking.clone()),
                ContentBlock::RedactedThinking { .. } => {
                    // Ignore redacted thinking blocks
                }
                _ => {
                    warn!(
                        "Unexpected content block type in assistant message: {:?}",
                        block
                    );
                }
            }
        }

        vec![OllamaMessage {
            role: "assistant".to_string(),
            content: content_parts.join("\n\n"),
            images: if images.is_empty() {
                None
            } else {
                Some(images)
            },
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
        }]
    }

    fn convert_user_message(blocks: &[ContentBlock]) -> Vec<OllamaMessage> {
        let mut messages = Vec::new();
        let mut current_content = Vec::new();
        let mut current_images = Vec::new();

        for block in blocks {
            match block {
                ContentBlock::ToolResult { content, .. } => {
                    // Add previous user content as separate message if any
                    if !current_content.is_empty() || !current_images.is_empty() {
                        messages.push(OllamaMessage {
                            role: "user".to_string(),
                            content: current_content.join("\n\n"),
                            images: if current_images.is_empty() {
                                None
                            } else {
                                Some(current_images.clone())
                            },
                            tool_calls: None,
                        });
                        current_content.clear();
                        current_images.clear();
                    }

                    // ToolResult as separate "tool" message
                    messages.push(OllamaMessage {
                        role: "tool".to_string(),
                        content: content.clone(),
                        images: None,
                        tool_calls: None,
                    });
                }
                ContentBlock::Text { text } => current_content.push(text.clone()),
                ContentBlock::Image { data, .. } => current_images.push(data.clone()),
                ContentBlock::Thinking { thinking, .. } => current_content.push(thinking.clone()),
                ContentBlock::RedactedThinking { .. } => {
                    // Ignore redacted thinking blocks
                }
                _ => {
                    warn!("Unexpected content block type in user message: {:?}", block);
                }
            }
        }

        // Add remaining user content if any
        if !current_content.is_empty() || !current_images.is_empty() {
            messages.push(OllamaMessage {
                role: "user".to_string(),
                content: current_content.join("\n\n"),
                images: if current_images.is_empty() {
                    None
                } else {
                    Some(current_images)
                },
                tool_calls: None,
            });
        }

        messages
    }

    async fn try_send_request(&self, request: &OllamaRequest) -> Result<LLMResponse> {
        let response = self
            .client
            .post(self.get_url())
            .json(request)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Network error: {}", e))?;

        // Store status code before consuming response
        let status = response.status();

        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(anyhow::anyhow!(
                "Ollama request failed: Status {}, Error: {}",
                status,
                error_text
            ));
        }

        let ollama_response: OllamaResponse = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse Ollama response: {}", e))?;

        let mut content = Vec::new();

        if !ollama_response.message.content.is_empty() {
            content.push(ContentBlock::Text {
                text: ollama_response.message.content,
            });
        }

        if let Some(tool_calls) = ollama_response.message.tool_calls {
            for (index, tool_call) in tool_calls.into_iter().enumerate() {
                content.push(ContentBlock::ToolUse {
                    id: format!("tool-{}-{}", tool_call.function.name, index),
                    name: tool_call.function.name,
                    input: tool_call.function.arguments,
                });
            }
        }

        Ok(LLMResponse {
            content,
            usage: Usage {
                input_tokens: ollama_response.prompt_eval_count,
                output_tokens: ollama_response.eval_count,
                // Ollama doesn't support caching, so these fields are 0
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            rate_limit_info: None,
        })
    }

    async fn try_send_request_streaming(
        &self,
        request: &OllamaRequest,
        streaming_callback: &StreamingCallback,
    ) -> Result<LLMResponse> {
        let response = self
            .client
            .post(self.get_url())
            .json(request)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Network error: {}", e))?;

        // Store status code before consuming response
        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(anyhow::anyhow!(
                "Ollama request failed: Status {}, Error: {}",
                status,
                error_text
            ));
        }

        let mut response = response;
        let mut line_buffer = String::new();
        let mut accumulated_content = String::new();
        let mut tool_calls = Vec::new();
        let mut final_eval_counts = (0u32, 0u32); // (prompt_eval_count, eval_count)

        while let Some(chunk) = response.chunk().await? {
            for byte in chunk {
                if byte == b'\n' {
                    if !line_buffer.is_empty() {
                        if let Ok(chunk_response) =
                            serde_json::from_str::<OllamaResponse>(&line_buffer)
                        {
                            // Handle text content
                            if !chunk_response.message.content.is_empty() {
                                streaming_callback(&StreamingChunk::Text(
                                    chunk_response.message.content.clone(),
                                ))?;
                                accumulated_content.push_str(&chunk_response.message.content);
                            }

                            // Handle tool calls - only collect complete tool calls from the response
                            if let Some(chunk_tool_calls) = chunk_response.message.tool_calls {
                                for tool_call in &chunk_tool_calls {
                                    // Stream the JSON input to the callback
                                    if let Ok(arguments_str) =
                                        serde_json::to_string(&tool_call.function.arguments)
                                    {
                                        streaming_callback(&StreamingChunk::InputJson {
                                            content: arguments_str,
                                            tool_name: Some(tool_call.function.name.clone()),
                                            tool_id: Some(format!(
                                                "tool-{}-{}",
                                                tool_call.function.name,
                                                tool_calls.len()
                                            )),
                                        })?;
                                    }
                                }
                                tool_calls.extend(chunk_tool_calls);
                            }

                            // Update eval counts from the final response
                            if chunk_response.done {
                                final_eval_counts =
                                    (chunk_response.prompt_eval_count, chunk_response.eval_count);
                            }
                        } else {
                            warn!("Failed to parse chunk line '{}'", line_buffer);
                        }
                        line_buffer.clear();
                    }
                } else {
                    line_buffer.push(byte as char);
                }
            }
        }

        // Send StreamingComplete to indicate streaming has finished
        streaming_callback(&StreamingChunk::StreamingComplete)?;

        // Build final response
        let mut content = Vec::new();

        // Add accumulated text content if present
        if !accumulated_content.is_empty() {
            content.push(ContentBlock::Text {
                text: accumulated_content,
            });
        }

        // Add tool calls if present
        for (index, tool_call) in tool_calls.into_iter().enumerate() {
            content.push(ContentBlock::ToolUse {
                id: format!("tool-{}-{}", tool_call.function.name, index),
                name: tool_call.function.name,
                input: tool_call.function.arguments,
            });
        }

        Ok(LLMResponse {
            content,
            usage: Usage {
                input_tokens: final_eval_counts.0,
                output_tokens: final_eval_counts.1,
                // Ollama doesn't support caching, so these fields are 0
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            rate_limit_info: None,
        })
    }
}

#[async_trait]
impl LLMProvider for OllamaClient {
    async fn send_message(
        &mut self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        let mut messages: Vec<OllamaMessage> = Vec::new();

        // Add system message
        messages.push(OllamaMessage {
            role: "system".to_string(),
            content: request.system_prompt,
            images: None,
            tool_calls: None,
        });

        // Add conversation messages
        for message in &request.messages {
            messages.extend(Self::convert_message(message));
        }

        let mut ollama_request = OllamaRequest {
            model: self.model.clone(),
            messages,
            stream: false,
            options: OllamaOptions {
                num_ctx: self.num_ctx,
            },
            tools: request.tools.map(|tools| {
                tools
                    .into_iter()
                    .map(|tool| {
                        serde_json::json!({
                            "type": "function",
                            "function": {
                                "name": tool.name,
                                "description": tool.description,
                                "parameters": tool.parameters
                            }
                        })
                    })
                    .collect()
            }),
        };

        debug!("Sending request to Ollama: {:?}", ollama_request);

        if let Some(callback) = streaming_callback {
            ollama_request.stream = true;
            self.try_send_request_streaming(&ollama_request, callback)
                .await
        } else {
            self.try_send_request(&ollama_request).await
        }
    }
}
