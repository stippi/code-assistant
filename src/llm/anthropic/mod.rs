use crate::llm::{
    rate_limits::send_with_retry, streaming::stream_response, types::*, LLMProvider,
    StreamingCallback,
};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use tracing::{debug, error};

mod rate_limits;
mod stream;
mod types;

use rate_limits::AnthropicRateLimitInfo;
use stream::AnthropicStreamHandler;
use types::*;

pub struct AnthropicClient {
    client: Client,
    api_key: String,
    base_url: String,
    model: String,
}

impl AnthropicClient {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url: "https://api.anthropic.com/v1".to_string(),
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
        format!("{}/messages", self.base_url)
    }

    async fn try_send_request(
        &self,
        request: &AnthropicRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<(LLMResponse, AnthropicRateLimitInfo)> {
        let accept_value = if streaming_callback.is_some() {
            "text/event-stream"
        } else {
            "application/json"
        };

        let mut response = self
            .client
            .post(&self.get_url())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("accept", accept_value)
            .json(request)
            .send()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        debug!("Response headers: {:?}", response.headers());

        // Extract rate limit information from response headers
        let rate_limits = AnthropicRateLimitInfo::from_response(&response);
        debug!("Parsed rate limits: {:?}", rate_limits);

        let status = response.status();
        if !status.is_success() {
            let response_text = response
                .text()
                .await
                .map_err(|e| ApiError::NetworkError(e.to_string()))?;

            // Try to parse the error response
            let error = if let Ok(error_response) =
                serde_json::from_str::<AnthropicErrorResponse>(&response_text)
            {
                match (status, error_response.error.error_type.as_str()) {
                    (StatusCode::TOO_MANY_REQUESTS, _) | (_, "rate_limit_error") => {
                        error!(
                            "Rate limit error detected: status={}, type={}, message={}",
                            status, error_response.error.error_type, error_response.error.message
                        );
                        ApiError::RateLimit(error_response.error.message)
                    }
                    (StatusCode::UNAUTHORIZED, _) => {
                        ApiError::Authentication(error_response.error.message)
                    }
                    (StatusCode::BAD_REQUEST, _) => {
                        ApiError::InvalidRequest(error_response.error.message)
                    }
                    (status, _) if status.is_server_error() => {
                        ApiError::ServiceError(error_response.error.message)
                    }
                    _ => {
                        error!(
                            "Unknown error detected: status={}, type={}, message={}",
                            status, error_response.error.error_type, error_response.error.message
                        );
                        ApiError::Unknown(error_response.error.message)
                    }
                }
            } else {
                ApiError::Unknown(format!(
                    "Failed to parse error response. Status {}: {}",
                    status, response_text
                ))
            };

            // Wrap the error with rate limit context
            return Err(ApiErrorContext {
                error,
                rate_limits: Some(rate_limits),
            }
            .into());
        }

        let response = if let Some(callback) = streaming_callback {
            let mut handler = AnthropicStreamHandler::new();
            stream_response(&mut response, &mut handler, callback).await?
        } else {
            let response_text = response
                .text()
                .await
                .map_err(|e| ApiError::NetworkError(e.to_string()))?;
            serde_json::from_str(&response_text)
                .map_err(|e| ApiError::Unknown(format!("Failed to parse response: {}", e)))?
        };

        Ok((response, rate_limits))
    }
}

#[async_trait]
impl LLMProvider for AnthropicClient {
    async fn send_message(
        &self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        let anthropic_request = AnthropicRequest {
            model: self.model.clone(),
            messages: request.messages,
            max_tokens: 8192,
            temperature: 0.7,
            system: Some(request.system_prompt),
            stream: streaming_callback.map(|_| true),
            tool_choice: match &request.tools {
                Some(_) => Some(serde_json::json!({
                    "type": "any",
                })),
                _ => None,
            },
            tools: request.tools.map(|tools| {
                tools
                    .into_iter()
                    .map(|tool| {
                        serde_json::json!({
                            "name": tool.name,
                            "description": tool.description,
                            "input_schema": tool.parameters
                        })
                    })
                    .collect()
            }),
        };

        let operation = || self.try_send_request(&anthropic_request, streaming_callback);
        send_with_retry(|| async { operation().await }, 3).await
    }
}
