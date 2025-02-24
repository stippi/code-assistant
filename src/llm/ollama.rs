use crate::llm::{types::*, LLMProvider, StreamingCallback};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

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
    pub fn new(model: String, num_ctx: usize) -> Self {
        Self {
            client: Client::new(),
            base_url: "http://localhost:11434".to_string(),
            model,
            num_ctx,
        }
    }

    #[cfg(test)]
    pub fn new_with_base_url(model: String, num_ctx: usize, base_url: String) -> Self {
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

    fn convert_message(message: &Message) -> OllamaMessage {
        OllamaMessage {
            role: match message.role {
                MessageRole::User => "user".to_string(),
                MessageRole::Assistant => "assistant".to_string(),
            },
            content: match &message.content {
                MessageContent::Text(text) => text.clone(),
                MessageContent::Structured(blocks) => {
                    // Concatenate all text blocks into the content string
                    blocks
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text { text } => Some(text),
                            _ => None,
                        })
                        .cloned()
                        .collect::<Vec<String>>()
                        .join("")
                }
            },
            tool_calls: match &message.content {
                MessageContent::Structured(blocks) => {
                    let tool_calls: Vec<OllamaToolCall> = blocks
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::ToolUse { name, input, .. } => Some(OllamaToolCall {
                                function: OllamaFunction {
                                    name: name.clone(),
                                    arguments: input.clone(),
                                },
                            }),
                            _ => None,
                        })
                        .collect();
                    if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    }
                }
                _ => None,
            },
        }
    }

    async fn try_send_request(&self, request: &OllamaRequest) -> Result<LLMResponse> {
        let response = self
            .client
            .post(&self.get_url())
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
                    id: format!("tool-{}", index),
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
            },
        })
    }

    async fn try_send_request_streaming(
        &self,
        request: &OllamaRequest,
        streaming_callback: &StreamingCallback,
    ) -> Result<LLMResponse> {
        let response = self
            .client
            .post(&self.get_url())
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
                                streaming_callback(&chunk_response.message.content)?;
                                accumulated_content.push_str(&chunk_response.message.content);
                            }

                            // Handle tool calls - only collect complete tool calls from the response
                            if let Some(chunk_tool_calls) = chunk_response.message.tool_calls {
                                tool_calls.extend(chunk_tool_calls);
                            }

                            // Update eval counts from the final response
                            if chunk_response.done {
                                final_eval_counts =
                                    (chunk_response.prompt_eval_count, chunk_response.eval_count);
                            }
                        }
                        line_buffer.clear();
                    }
                } else {
                    line_buffer.push(byte as char);
                }
            }
        }

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
                id: format!("tool-{}", index),
                name: tool_call.function.name,
                input: tool_call.function.arguments,
            });
        }

        Ok(LLMResponse {
            content,
            usage: Usage {
                input_tokens: final_eval_counts.0,
                output_tokens: final_eval_counts.1,
            },
        })
    }
}

#[async_trait]
impl LLMProvider for OllamaClient {
    async fn send_message(
        &self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        let mut messages: Vec<OllamaMessage> = Vec::new();

        // Add system message
        messages.push(OllamaMessage {
            role: "system".to_string(),
            content: request.system_prompt,
            tool_calls: None,
        });

        // Add conversation messages
        messages.extend(request.messages.iter().map(Self::convert_message));

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
