use crate::llm::{
    types::*, ApiError, ApiErrorContext, LLMProvider, RateLimitHandler, StreamingCallback,
};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, trace, warn};

#[derive(Debug, Serialize)]
struct VertexRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<SystemInstruction>,
    contents: Vec<VertexMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GenerationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_config: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct SystemInstruction {
    parts: Parts,
}

#[derive(Debug, Serialize)]
struct Parts {
    text: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct VertexMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<VertexPart>,
}

#[derive(Debug, Serialize, Deserialize)]
struct VertexPart {
    #[serde(rename = "functionCall")]
    function_call: Option<VertexFunctionCall>,
    // Optional text field could be added if we get text responses
    text: Option<String>,
}

#[derive(Debug, Serialize)]
struct GenerationConfig {
    temperature: f32,
    max_output_tokens: usize,
}

#[derive(Debug, Deserialize)]
struct VertexResponse {
    candidates: Vec<VertexCandidate>,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<VertexUsageMetadata>,
}

#[derive(Debug, Deserialize)]
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
struct VertexFunctionCall {
    name: String,
    args: serde_json::Value,
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
    #[allow(dead_code)]
    requests_reset: Option<Duration>,
}

impl RateLimitHandler for VertexRateLimitInfo {
    fn from_response(_response: &Response) -> Self {
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

    async fn send_with_retry(
        &self,
        request: &VertexRequest,
        streaming_callback: Option<&StreamingCallback>,
        max_retries: u32,
    ) -> Result<LLMResponse> {
        let mut attempts = 0;

        loop {
            match if let Some(callback) = streaming_callback {
                self.try_send_request_streaming(request, callback).await
            } else {
                self.try_send_request(request).await
            } {
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

    async fn handle_error_response(
        &self,
        response: Response,
    ) -> Result<(LLMResponse, VertexRateLimitInfo)> {
        let rate_limits = VertexRateLimitInfo::from_response(&response);
        let status = response.status();
        let response_text = response
            .text()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

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

        Err(ApiErrorContext {
            error,
            rate_limits: Some(rate_limits),
        }
        .into())
    }

    async fn try_send_request(
        &self,
        request: &VertexRequest,
    ) -> Result<(LLMResponse, VertexRateLimitInfo)> {
        let url = self.get_url(false);

        trace!(
            "Sending Vertex request to {}:\n{}",
            self.model,
            serde_json::to_string_pretty(request)?
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

        if !response.status().is_success() {
            return self.handle_error_response(response).await;
        }

        let rate_limits = VertexRateLimitInfo::from_response(&response);

        trace!("Response headers: {:?}", response.headers());

        let response_text = response
            .text()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        trace!(
            "Vertex response: {}",
            serde_json::to_string_pretty(&serde_json::from_str::<serde_json::Value>(
                &response_text
            )?)?
        );

        let vertex_response: VertexResponse = serde_json::from_str(&response_text)
            .map_err(|e| ApiError::Unknown(format!("Failed to parse response: {}", e)))?;

        // Convert to our generic LLMResponse format
        let response = LLMResponse {
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
                                    id: format!("tool-{}", index), // Generate a unique ID
                                    name: function_call.name,
                                    input: function_call.args,
                                }
                            } else if let Some(text) = part.text {
                                ContentBlock::Text { text }
                            } else {
                                // Fallback if neither function_call nor text is present
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
        };

        Ok((response, rate_limits))
    }

    async fn try_send_request_streaming(
        &self,
        request: &VertexRequest,
        streaming_callback: &StreamingCallback,
    ) -> Result<(LLMResponse, VertexRateLimitInfo)> {
        let mut response = self
            .client
            .post(&self.get_url(true))
            .query(&[("key", &self.api_key), ("alt", &"sse".to_string())])
            .header("Content-Type", "application/json")
            .json(request)
            .send()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        if !response.status().is_success() {
            return self.handle_error_response(response).await;
        }

        let rate_limits = VertexRateLimitInfo::from_response(&response);

        let mut content_blocks = Vec::new();
        let mut current_text = String::new();
        let mut last_usage: Option<VertexUsageMetadata> = None;
        let mut line_buffer = String::new();

        while let Some(chunk) = response.chunk().await? {
            let chunk_str = std::str::from_utf8(&chunk)?;

            for c in chunk_str.chars() {
                if c == '\n' {
                    if !line_buffer.is_empty() {
                        if let Some(data) = line_buffer.strip_prefix("data: ") {
                            if let Ok(response) = serde_json::from_str::<VertexResponse>(data) {
                                if let Some(candidate) = response.candidates.first() {
                                    for part in &candidate.content.parts {
                                        if let Some(text) = &part.text {
                                            streaming_callback(text)?;
                                            current_text.push_str(text);
                                        } else if let Some(function_call) = &part.function_call {
                                            // If we have accumulated text, push it as a content block
                                            if !current_text.is_empty() {
                                                content_blocks.push(ContentBlock::Text {
                                                    text: current_text.clone(),
                                                });
                                                current_text.clear();
                                            }

                                            content_blocks.push(ContentBlock::ToolUse {
                                                id: format!("tool-{}", content_blocks.len()),
                                                name: function_call.name.clone(),
                                                input: function_call.args.clone(),
                                            });
                                        }
                                    }
                                }
                                if let Some(usage) = response.usage_metadata {
                                    last_usage = Some(usage);
                                }
                            }
                        }
                        line_buffer.clear();
                    }
                } else {
                    line_buffer.push(c);
                }
            }
        }

        // Process any remaining data in the buffer
        if !line_buffer.is_empty() {
            if let Some(data) = line_buffer.strip_prefix("data: ") {
                if let Ok(response) = serde_json::from_str::<VertexResponse>(data) {
                    if let Some(usage) = response.usage_metadata {
                        last_usage = Some(usage);
                    }
                }
            }
        }

        // Push any remaining text as a final content block
        if !current_text.is_empty() {
            content_blocks.push(ContentBlock::Text { text: current_text });
        }

        Ok((
            LLMResponse {
                content: content_blocks,
                usage: Usage {
                    input_tokens: last_usage
                        .as_ref()
                        .map(|u| u.prompt_token_count)
                        .unwrap_or(0),
                    output_tokens: last_usage
                        .as_ref()
                        .map(|u| u.candidates_token_count)
                        .unwrap_or(0),
                },
            },
            rate_limits,
        ))
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
                vec![json!({
                    "function_declarations": tools.into_iter().map(|tool| {
                        json!({
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                        })
                    }).collect::<Vec<_>>()
                })]
            }),
            tool_config: Some(json!({
                "function_calling_config": {
                    "mode": "ANY",
                }
            })),
        };

        self.send_with_retry(&vertex_request, streaming_callback, 3)
            .await
    }
}
