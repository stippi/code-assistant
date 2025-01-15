use crate::llm::{types::*, ApiError, ApiErrorContext, LLMProvider, RateLimitHandler};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, warn};

#[derive(Debug, Serialize)]
struct VertexRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<SystemInstruction>,
    contents: Vec<VertexMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GenerationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<ToolsWrapper>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_config: Option<ToolConfig>,
}

#[derive(Debug, Serialize)]
struct SystemInstruction {
    parts: Parts,
}

#[derive(Debug, Serialize)]
struct Parts {
    text: String,
}

#[derive(Debug, Serialize)]
struct VertexMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<MessagePart>,
}

#[derive(Debug, Serialize)]
struct MessagePart {
    text: String,
}

#[derive(Debug, Serialize)]
struct GenerationConfig {
    temperature: f32,
    max_output_tokens: usize,
}

#[derive(Debug, Serialize)]
struct ToolConfig {
    function_calling_config: FunctionCallingConfig,
}

#[derive(Debug, Serialize)]
struct FunctionCallingConfig {
    mode: String,
}

#[derive(Debug, Serialize)]
struct ToolsWrapper {
    function_declarations: Vec<ToolDefinition>,
}

#[derive(Debug, Deserialize)]
struct VertexResponse {
    candidates: Vec<VertexCandidate>,
}

#[derive(Debug, Deserialize)]
struct VertexCandidate {
    content: VertexContent,
}

#[derive(Debug, Deserialize)]
struct VertexContent {
    parts: Vec<MessagePart>,
    // TODO: Add function call response fields once we know the exact format
}

#[derive(Debug, Deserialize)]
struct VertexErrorResponse {
    error: VertexError,
}

#[derive(Debug, Deserialize)]
struct VertexError {
    message: String,
    code: Option<i32>,
}

/// Rate limit information extracted from response headers
#[derive(Debug)]
struct VertexRateLimitInfo {
    // TODO: Add actual rate limit fields once we know what headers Vertex AI uses
    requests_remaining: Option<u32>,
    requests_reset: Option<Duration>,
}

impl RateLimitHandler for VertexRateLimitInfo {
    fn from_response(response: &Response) -> Self {
        // TODO: Parse actual rate limit headers once we know what Vertex AI provides
        Self {
            requests_remaining: None,
            requests_reset: None,
        }
    }

    fn get_retry_delay(&self) -> Duration {
        // Default exponential backoff strategy
        Duration::from_secs(2)
    }

    fn log_status(&self) {
        debug!(
            "Vertex AI Rate limits - Requests remaining: {}",
            self.requests_remaining
                .map_or("unknown".to_string(), |r| r.to_string())
        );
    }
}

pub struct VertexClient {
    client: Client,
    api_key: String,
    model: String,
}

impl VertexClient {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
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
            parts: vec![MessagePart { text }],
        }
    }

    async fn send_with_retry(
        &self,
        request: &VertexRequest,
        max_retries: u32,
    ) -> Result<LLMResponse> {
        let mut attempts = 0;

        loop {
            match self.try_send_request(request).await {
                Ok((response, rate_limits)) => {
                    rate_limits.log_status();
                    return Ok(response);
                }
                Err(e) => {
                    let rate_limits = e
                        .downcast_ref::<ApiErrorContext<VertexRateLimitInfo>>()
                        .and_then(|ctx| ctx.rate_limits.as_ref());

                    match e.downcast_ref::<ApiError>() {
                        Some(ApiError::RateLimit(_)) => {
                            if attempts < max_retries {
                                attempts += 1;
                                let delay = rate_limits
                                    .map(|r| r.get_retry_delay())
                                    .unwrap_or_else(|| Duration::from_secs(2u64.pow(attempts)));
                                warn!(
                                    "Vertex AI rate limit hit (attempt {}/{}), waiting {} seconds before retry",
                                    attempts,
                                    max_retries,
                                    delay.as_secs()
                                );
                                sleep(delay).await;
                                continue;
                            }
                        }
                        Some(ApiError::ServiceError(_)) | Some(ApiError::NetworkError(_)) => {
                            if attempts < max_retries {
                                attempts += 1;
                                let delay = Duration::from_secs(2u64.pow(attempts - 1));
                                warn!(
                                    "Error: {} (attempt {}/{}), retrying in {} seconds",
                                    e,
                                    attempts,
                                    max_retries,
                                    delay.as_secs()
                                );
                                sleep(delay).await;
                                continue;
                            }
                        }
                        _ => {} // Don't retry other types of errors
                    }
                    return Err(e);
                }
            }
        }
    }

    async fn try_send_request(
        &self,
        request: &VertexRequest,
    ) -> Result<(LLMResponse, VertexRateLimitInfo)> {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
            self.model
        );

        let response = self
            .client
            .post(&url)
            .query(&[("key", &self.api_key)])
            .header("Content-Type", "application/json")
            .json(request)
            .send()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        let rate_limits = VertexRateLimitInfo::from_response(&response);

        let status = response.status();
        let response_text = response
            .text()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        if !status.is_success() {
            let error = if let Ok(error_response) =
                serde_json::from_str::<VertexErrorResponse>(&response_text)
            {
                match (status, error_response.error.code) {
                    (StatusCode::TOO_MANY_REQUESTS, _) => {
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
                    _ => ApiError::Unknown(error_response.error.message),
                }
            } else {
                ApiError::Unknown(format!("Status {}: {}", status, response_text))
            };

            return Err(ApiErrorContext {
                error,
                rate_limits: Some(rate_limits),
            }
            .into());
        }

        let vertex_response: VertexResponse = serde_json::from_str(&response_text)
            .map_err(|e| ApiError::Unknown(format!("Failed to parse response: {}", e)))?;

        // Convert to our generic LLMResponse format
        let response = LLMResponse {
            content: vec![ContentBlock::Text {
                text: vertex_response.candidates[0]
                    .content
                    .parts
                    .first()
                    .map(|p| p.text.clone())
                    .unwrap_or_default(),
            }],
        };

        Ok((response, rate_limits))
    }
}

#[async_trait]
impl LLMProvider for VertexClient {
    async fn send_message(&self, request: LLMRequest) -> Result<LLMResponse> {
        let mut contents = Vec::new();

        // Convert messages
        contents.extend(request.messages.iter().map(Self::convert_message));

        let vertex_request = VertexRequest {
            system_instruction: request.system_prompt.map(|prompt| SystemInstruction {
                parts: Parts { text: prompt },
            }),
            contents,
            generation_config: Some(GenerationConfig {
                temperature: request.temperature,
                max_output_tokens: request.max_tokens,
            }),
            tools: request.tools.map(|tools| ToolsWrapper {
                function_declarations: tools,
            }),
            tool_config: Some(ToolConfig {
                function_calling_config: FunctionCallingConfig {
                    mode: "none".to_string(), // TODO: Update based on tools presence
                },
            }),
        };

        self.send_with_retry(&vertex_request, 3).await
    }
}
