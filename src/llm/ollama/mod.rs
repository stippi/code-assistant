use crate::llm::{
    rate_limits::{check_response_error, send_with_retry},
    streaming::stream_response,
    types::*,
    LLMProvider, StreamingCallback,
};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use tracing::debug;

mod rate_limits;
mod stream;
mod types;

use rate_limits::OllamaRateLimitInfo;
use stream::OllamaStreamHandler;
use types::*;

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
                MessageContent::Structured(_) => "[Structured content not supported]".to_string(),
            },
        }
    }

    async fn try_send_request(
        &self,
        request: &OllamaRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<(LLMResponse, OllamaRateLimitInfo)> {
        let mut response = self
            .client
            .post(&self.get_url())
            .json(request)
            .send()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        response = check_response_error::<OllamaRateLimitInfo>(response).await?;
        let rate_limits = OllamaRateLimitInfo::from_response(&response);

        let response = if let Some(callback) = streaming_callback {
            let mut handler = OllamaStreamHandler::new();
            stream_response(&mut response, &mut handler, callback).await?
        } else {
            let ollama_response: OllamaResponse = response
                .json()
                .await
                .map_err(|e| ApiError::Unknown(format!("Failed to parse response: {}", e)))?;

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

            LLMResponse {
                content,
                usage: Usage {
                    input_tokens: ollama_response.prompt_eval_count,
                    output_tokens: ollama_response.eval_count,
                },
            }
        };

        Ok((response, rate_limits))
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
        });

        // Add conversation messages
        messages.extend(request.messages.iter().map(Self::convert_message));

        let ollama_request = OllamaRequest {
            model: self.model.clone(),
            messages,
            stream: streaming_callback.is_some(),
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

        let operation = || self.try_send_request(&ollama_request, streaming_callback);
        send_with_retry(|| async { operation().await }, 3).await
    }
}
