use crate::llm::{types::*, ApiError, ApiErrorContext, LLMProvider, RateLimitHandler};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, warn};

#[derive(Debug, Serialize)]
struct OpenAIRequest {
    model: String,
    messages: Vec<OpenAIChatMessage>,
    temperature: f32,
    max_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenAIChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIResponse {
    choices: Vec<OpenAIChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    message: OpenAIChatMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAIErrorResponse {
    error: OpenAIError,
}

#[derive(Debug, Deserialize)]
struct OpenAIError {
    message: String,
    #[serde(rename = "type")]
    code: Option<String>,
}

/// Rate limit information extracted from response headers
#[derive(Debug)]
struct OpenAIRateLimitInfo {
    requests_limit: Option<u32>,
    requests_remaining: Option<u32>,
    requests_reset: Option<Duration>,
    tokens_limit: Option<u32>,
    tokens_remaining: Option<u32>,
    tokens_reset: Option<Duration>,
}

impl RateLimitHandler for OpenAIRateLimitInfo {
    fn from_response(response: &Response) -> Self {
        let headers = response.headers();

        fn parse_header<T: std::str::FromStr>(
            headers: &reqwest::header::HeaderMap,
            name: &str,
        ) -> Option<T> {
            headers
                .get(name)
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.parse().ok())
        }

        fn parse_duration(headers: &reqwest::header::HeaderMap, name: &str) -> Option<Duration> {
            headers
                .get(name)
                .and_then(|h| h.to_str().ok())
                .and_then(|s| {
                    // Parse OpenAI's duration format (e.g., "1s", "6m0s")
                    let mut seconds = 0u64;
                    let mut current_num = String::new();

                    for c in s.chars() {
                        match c {
                            '0'..='9' => current_num.push(c),
                            'm' => {
                                if let Ok(mins) = current_num.parse::<u64>() {
                                    seconds += mins * 60;
                                }
                                current_num.clear();
                            }
                            's' => {
                                if let Ok(secs) = current_num.parse::<u64>() {
                                    seconds += secs;
                                }
                                current_num.clear();
                            }
                            _ => current_num.clear(),
                        }
                    }
                    Some(Duration::from_secs(seconds))
                })
        }

        Self {
            requests_limit: parse_header(headers, "x-ratelimit-limit-requests"),
            requests_remaining: parse_header(headers, "x-ratelimit-remaining-requests"),
            requests_reset: parse_duration(headers, "x-ratelimit-reset-requests"),
            tokens_limit: parse_header(headers, "x-ratelimit-limit-tokens"),
            tokens_remaining: parse_header(headers, "x-ratelimit-remaining-tokens"),
            tokens_reset: parse_duration(headers, "x-ratelimit-reset-tokens"),
        }
    }

    fn get_retry_delay(&self) -> Duration {
        // Take the longer of the two reset times if both are present
        let mut delay = Duration::from_secs(2); // Default fallback

        if let Some(requests_reset) = self.requests_reset {
            delay = delay.max(requests_reset);
        }

        if let Some(tokens_reset) = self.tokens_reset {
            delay = delay.max(tokens_reset);
        }

        // Add a small buffer
        delay + Duration::from_secs(1)
    }

    fn log_status(&self) {
        debug!(
            "OpenAI Rate limits - Requests: {}/{} (reset in: {}s), Tokens: {}/{} (reset in: {}s)",
            self.requests_remaining
                .map_or("?".to_string(), |r| r.to_string()),
            self.requests_limit
                .map_or("?".to_string(), |l| l.to_string()),
            self.requests_reset.map_or(0, |d| d.as_secs()),
            self.tokens_remaining
                .map_or("?".to_string(), |r| r.to_string()),
            self.tokens_limit.map_or("?".to_string(), |l| l.to_string()),
            self.tokens_reset.map_or(0, |d| d.as_secs()),
        );
    }
}

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
            base_url: "https://api.openai.com/v1/chat/completions".to_string(),
            model,
        }
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
                    // For now, we'll just convert structured content to a simple text message
                    // This could be enhanced to handle OpenAI's specific formats
                    "[Structured content not supported]".to_string()
                }
            },
        }
    }

    async fn send_with_retry(
        &self,
        request: &OpenAIRequest,
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
                        .downcast_ref::<ApiErrorContext<OpenAIRateLimitInfo>>()
                        .and_then(|ctx| ctx.rate_limits.as_ref());

                    match e.downcast_ref::<ApiError>() {
                        Some(ApiError::RateLimit(_)) => {
                            if let Some(rate_limits) = rate_limits {
                                if attempts < max_retries {
                                    attempts += 1;
                                    let delay = rate_limits.get_retry_delay();
                                    warn!(
                                        "OpenAI rate limit hit (attempt {}/{}), waiting {} seconds before retry",
                                        attempts,
                                        max_retries,
                                        delay.as_secs()
                                    );
                                    sleep(delay).await;
                                    continue;
                                }
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
        request: &OpenAIRequest,
    ) -> Result<(LLMResponse, OpenAIRateLimitInfo)> {
        let response = self
            .client
            .post(&self.base_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(request)
            .send()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        let rate_limits = OpenAIRateLimitInfo::from_response(&response);

        let status = response.status();
        let response_text = response
            .text()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        if !status.is_success() {
            let error = if let Ok(error_response) =
                serde_json::from_str::<OpenAIErrorResponse>(&response_text)
            {
                match (status, error_response.error.code.as_deref()) {
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

        // Parse the successful response
        let openai_response: OpenAIResponse = serde_json::from_str(&response_text)
            .map_err(|e| ApiError::Unknown(format!("Failed to parse response: {}", e)))?;

        // Convert to our generic LLMResponse format
        // TODO: Handle tools
        let response = LLMResponse {
            content: vec![ContentBlock::Text {
                text: openai_response.choices[0].message.content.clone(),
            }],
        };

        Ok((response, rate_limits))
    }
}

#[async_trait]
impl LLMProvider for OpenAIClient {
    async fn send_message(&self, request: LLMRequest) -> Result<LLMResponse> {
        let mut messages: Vec<OpenAIChatMessage> = Vec::new();

        // Add system message if present
        if let Some(system_prompt) = request.system_prompt {
            messages.push(OpenAIChatMessage {
                role: "system".to_string(),
                content: system_prompt,
            });
        }

        // Add conversation messages
        messages.extend(request.messages.iter().map(Self::convert_message));

        let openai_request = OpenAIRequest {
            model: self.model.clone(),
            messages,
            temperature: request.temperature,
            max_tokens: Some(request.max_tokens),
            stream: None,
            tools: request.tools.map(|tools| {
                tools
                    .into_iter()
                    .map(|tool| {
                        serde_json::json!({
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters
                        })
                    })
                    .collect()
            }),
        };

        self.send_with_retry(&openai_request, 3).await
    }
}
