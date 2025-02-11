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

#[derive(Debug, Serialize)]
struct OllamaMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct OllamaResponse {
    message: OllamaResponseMessage,
    #[allow(dead_code)]
    done_reason: Option<String>,
    #[allow(dead_code)]
    done: bool,
    prompt_eval_count: u32,
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

#[derive(Debug, Deserialize)]
struct OllamaResponseMessage {
    content: String,
    tool_calls: Option<Vec<OllamaToolCall>>,
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
            base_url: "http://localhost:11434/api/chat".to_string(),
            model,
            num_ctx,
        }
    }

    fn convert_message(message: &Message) -> OllamaMessage {
        OllamaMessage {
            role: match message.role {
                MessageRole::User => "user".to_string(),
                MessageRole::Assistant => "assistant".to_string(),
            },
            content: match &message.content {
                MessageContent::Text(text) => text.clone(),
                MessageContent::Structured(_) => "[Structured content not supported]".to_string(),
            },
        }
    }

    async fn try_send_request(&self, request: &OllamaRequest) -> Result<OllamaResponse> {
        let response = self
            .client
            .post(&self.base_url)
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

        let ollama_response = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse Ollama response: {}", e))?;

        Ok(ollama_response)
    }
}

#[async_trait]
impl LLMProvider for OllamaClient {
    async fn send_message(
        &self,
        request: LLMRequest,
        _streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        let mut messages: Vec<OllamaMessage> = Vec::new();

        // Add system message
        messages.push(OllamaMessage {
            role: "system".to_string(),
            content: request.system_prompt,
        });

        // Add conversation messages
        messages.extend(request.messages.iter().map(Self::convert_message));

        let ollama_request = OllamaRequest {
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

        let response = self.try_send_request(&ollama_request).await?;

        let mut content = Vec::new();

        if !response.message.content.is_empty() {
            content.push(ContentBlock::Text {
                text: response.message.content,
            });
        }

        if let Some(tool_calls) = response.message.tool_calls {
            for (index, tool_call) in tool_calls.into_iter().enumerate() {
                content.push(ContentBlock::ToolUse {
                    id: format!("ollama-{}", index),
                    name: tool_call.function.name,
                    input: tool_call.function.arguments,
                });
            }
        }

        Ok(LLMResponse {
            content,
            usage: Usage {
                input_tokens: response.prompt_eval_count,
                output_tokens: response.eval_count,
            },
        })
    }
}
