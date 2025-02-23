
use crate::llm::{
    rate_limits::{check_response_error, send_with_retry},
    streaming::stream_response,
    types::*,
    LLMProvider,
    StreamingCallback,
};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use tracing::debug;

mod rate_limits;
mod stream;
mod types;

use rate_limits::OpenAIRateLimitInfo;
use stream::OpenAIStreamHandler;
use types::*;

pub struct OpenAIClient {
    client: Client,
    api_key: String,
    base_url: String,
    model: String,
}

impl OpenAIClient {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url: "https://api.openai.com/v1".to_string(),
            model,
        }
    }

    #[cfg(test)]
    pub fn new_with_base_url(api_key: String, model: String, base_url: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url,
            model,
        }
    }

    fn get_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    fn convert_message(message: &Message) -> OpenAIChatMessage {
        OpenAIChatMessage {
            role: match message.role {
                MessageRole::User => "user".to_string(),
                MessageRole::Assistant => "assistant".to_string(),
            },
            content: match &message.content {
                MessageContent::Text(text) => text.clone(),
                MessageContent::Structured(_) => {
                    "[Structured content not supported]".to_string()
                }
            },
            tool_calls: None,
        }
    }

    async fn try_send_request(
        &self,
        request: &OpenAIRequest,
    ) -> Result<(LLMResponse, OpenAIRateLimitInfo)> {
        let request = request.clone().into_non_streaming();
        let response = self
            .client
            .post(&self.get_url())
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        let response = check_response_error::<OpenAIRateLimitInfo>(response).await?;
        let rate_limits = OpenAIRateLimitInfo::from_response(&response);

        let response_text = response
            .text()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        debug!("OpenAI response: {}", response_text);

        // Parse the successful response
        let openai_response: OpenAIResponse = serde_json::from_str(&response_text)
            .map_err(|e| ApiError::Unknown(format!("Failed to parse response: {}", e)))?;

        // Convert to our generic LLMResponse format
        Ok((
            LLMResponse {
                content: {
                    let mut blocks = Vec::new();

                    // Add text content if present
                    if !openai_response.choices[0].message.content.is_empty() {
                        blocks.push(ContentBlock::Text {
                            text: openai_response.choices[0].message.content.clone(),
                        });
                    }

                    // Add tool calls if present
                    if let Some(ref tool_calls) = openai_response.choices[0].message.tool_calls {
                        for call in tool_calls {
                            let input =
                                serde_json::from_str(&call.function.arguments).map_err(|e| {
                                    ApiError::Unknown(format!(
                                        "Failed to parse tool arguments: {}",
                                        e
                                    ))
                                })?;
                            blocks.push(ContentBlock::ToolUse {
                                id: call.id.clone(),
                                name: call.function.name.clone(),
                                input,
                            });
                        }
                    }

                    blocks
                },
                usage: Usage {
                    input_tokens: openai_response.usage.prompt_tokens,
                    output_tokens: openai_response.usage.completion_tokens,
                },
            },
            rate_limits,
        ))
    }

    async fn try_send_request_streaming(
        &self,
        request: &OpenAIRequest,
        streaming_callback: &StreamingCallback,
    ) -> Result<(LLMResponse, OpenAIRateLimitInfo)> {
        debug!("Sending streaming request");
        let request = request.clone().into_streaming();
        let mut response = self
            .client
            .post(&self.get_url())
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        response = check_response_error::<OpenAIRateLimitInfo>(response).await?;
        let rate_limits = OpenAIRateLimitInfo::from_response(&response);

        let mut handler = OpenAIStreamHandler::new();
        let response = stream_response(&mut response, &mut handler, streaming_callback).await?;

        Ok((response, rate_limits))
    }
}

#[async_trait]
impl LLMProvider for OpenAIClient {
    async fn send_message(
        &self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        let mut messages: Vec<OpenAIChatMessage> = Vec::new();

        // Add system message
        messages.push(OpenAIChatMessage {
            role: "system".to_string(),
            content: request.system_prompt,
            tool_calls: None,
        });

        // Add conversation messages
        messages.extend(request.messages.iter().map(Self::convert_message));

        let openai_request = OpenAIRequest {
            model: self.model.clone(),
            messages,
            temperature: 1.0,
            stream: None,
            stream_options: None,
            tool_choice: match &request.tools {
                Some(_) => Some(serde_json::json!("required")),
                _ => None,
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

        let operation = || async {
            if let Some(callback) = streaming_callback {
                self.try_send_request_streaming(&openai_request, callback).await
            } else {
                self.try_send_request(&openai_request).await
            }
        };

        send_with_retry(operation, 3).await
    }
}
