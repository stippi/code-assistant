use crate::llm::{
    rate_limits::{check_response_error, send_with_retry},
    streaming::stream_response,
    types::*,
    LLMProvider, StreamingCallback,
};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use tracing::trace;

mod rate_limits;
mod stream;
mod types;

use rate_limits::VertexRateLimitInfo;
use stream::VertexStreamHandler;
use types::*;

pub struct VertexClient {
    client: Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl VertexClient {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
            base_url: "https://generativelanguage.googleapis.com/v1beta".to_string(),
        }
    }

    #[cfg(test)]
    pub fn new_with_base_url(api_key: String, model: String, base_url: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
            base_url,
        }
    }

    fn get_url(&self, streaming: bool) -> String {
        if streaming {
            format!(
                "{}/models/{}:streamGenerateContent",
                self.base_url, self.model
            )
        } else {
            format!("{}/models/{}:generateContent", self.base_url, self.model)
        }
    }

    fn convert_message(message: &Message) -> VertexMessage {
        let text = match &message.content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Structured(_) => "[Structured content not supported]".to_string(),
        };

        VertexMessage {
            role: Some(match message.role {
                MessageRole::User => "user".to_string(),
                MessageRole::Assistant => "model".to_string(),
            }),
            parts: vec![VertexPart {
                text: Some(text),
                function_call: None,
            }],
        }
    }

    async fn try_send_request(
        &self,
        request: &VertexRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<(LLMResponse, VertexRateLimitInfo)> {
        let mut response = self
            .client
            .post(&self.get_url(streaming_callback.is_some()))
            .query(&[("key", &self.api_key)])
            .header("Content-Type", "application/json")
            .json(request)
            .send()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        response = check_response_error::<VertexRateLimitInfo>(response).await?;
        let rate_limits = VertexRateLimitInfo::from_response(&response);

        trace!("Response headers: {:?}", response.headers());

        let response = if let Some(callback) = streaming_callback {
            let mut handler = VertexStreamHandler::new();
            stream_response(&mut response, &mut handler, callback).await?
        } else {
            let response_text = response
                .text()
                .await
                .map_err(|e| ApiError::NetworkError(e.to_string()))?;

            let vertex_response: VertexResponse = serde_json::from_str(&response_text)
                .map_err(|e| ApiError::Unknown(format!("Failed to parse response: {}", e)))?;

            LLMResponse {
                content: vertex_response
                    .candidates
                    .into_iter()
                    .flat_map(|candidate| {
                        candidate
                            .content
                            .parts
                            .into_iter()
                            .enumerate()
                            .map(|(index, part)| {
                                if let Some(function_call) = part.function_call {
                                    ContentBlock::ToolUse {
                                        id: format!("tool-{}", index),
                                        name: function_call.name,
                                        input: function_call.args,
                                    }
                                } else if let Some(text) = part.text {
                                    ContentBlock::Text { text }
                                } else {
                                    ContentBlock::Text {
                                        text: "Empty response part".to_string(),
                                    }
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                    .collect(),
                usage: Usage {
                    input_tokens: vertex_response
                        .usage_metadata
                        .as_ref()
                        .map(|u| u.prompt_token_count)
                        .unwrap_or(0),
                    output_tokens: vertex_response
                        .usage_metadata
                        .as_ref()
                        .map(|u| u.candidates_token_count)
                        .unwrap_or(0),
                },
            }
        };

        Ok((response, rate_limits))
    }
}

#[async_trait]
impl LLMProvider for VertexClient {
    async fn send_message(
        &self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        let mut contents = Vec::new();

        // Convert messages
        contents.extend(request.messages.iter().map(Self::convert_message));

        let vertex_request = VertexRequest {
            system_instruction: Some(SystemInstruction {
                parts: Parts {
                    text: request.system_prompt,
                },
            }),
            contents,
            generation_config: Some(GenerationConfig {
                temperature: 0.7,
                max_output_tokens: 8192,
            }),
            tools: request.tools.map(|tools| {
                vec![serde_json::json!({
                    "function_declarations": tools.into_iter().map(|tool| {
                        serde_json::json!({
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                        })
                    }).collect::<Vec<_>>()
                })]
            }),
            tool_config: Some(serde_json::json!({
                "function_calling_config": {
                    "mode": "ANY",
                }
            })),
        };

        let operation = || self.try_send_request(&vertex_request, streaming_callback);
        send_with_retry(|| async { operation().await }, 3).await
    }
}
