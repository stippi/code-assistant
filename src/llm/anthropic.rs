use crate::llm::{
    types::*, ApiError, ApiErrorContext, LLMProvider, RateLimitHandler, StreamingCallback,
};
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::str::{self};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, error, warn};

/// Response structure for Anthropic error messages
#[derive(Debug, Serialize, serde::Deserialize)]
struct AnthropicErrorResponse {
    #[serde(rename = "type")]
    error_type: String,
    error: AnthropicErrorPayload,
}

#[derive(Debug, Serialize, serde::Deserialize)]
struct AnthropicErrorPayload {
    #[serde(rename = "type")]
    error_type: String,
    message: String,
}

/// Rate limit information extracted from response headers
#[derive(Debug)]
struct AnthropicRateLimitInfo {
    requests_limit: Option<u32>,
    requests_remaining: Option<u32>,
    requests_reset: Option<DateTime<Utc>>,
    tokens_limit: Option<u32>,
    tokens_remaining: Option<u32>,
    tokens_reset: Option<DateTime<Utc>>,
    retry_after: Option<Duration>,
}

impl RateLimitHandler for AnthropicRateLimitInfo {
    /// Extract rate limit information from response headers
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

        fn parse_datetime(
            headers: &reqwest::header::HeaderMap,
            name: &str,
        ) -> Option<DateTime<Utc>> {
            headers
                .get(name)
                .and_then(|h| h.to_str().ok())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.into())
        }

        Self {
            requests_limit: parse_header(headers, "anthropic-ratelimit-requests-limit"),
            requests_remaining: parse_header(headers, "anthropic-ratelimit-requests-remaining"),
            requests_reset: parse_datetime(headers, "anthropic-ratelimit-requests-reset"),
            tokens_limit: parse_header(headers, "anthropic-ratelimit-tokens-limit"),
            tokens_remaining: parse_header(headers, "anthropic-ratelimit-tokens-remaining"),
            tokens_reset: parse_datetime(headers, "anthropic-ratelimit-tokens-reset"),
            retry_after: parse_header::<u64>(headers, "retry-after").map(Duration::from_secs),
        }
    }

    /// Calculate how long to wait before retrying based on rate limit information
    fn get_retry_delay(&self) -> Duration {
        // If we have a specific retry-after duration, use that
        if let Some(retry_after) = self.retry_after {
            return retry_after;
        }

        // Otherwise, calculate based on reset times
        let now = Utc::now();
        let mut shortest_wait = Duration::from_secs(60); // Default to 60 seconds if no information

        // Check requests reset time
        if let Some(reset_time) = self.requests_reset {
            if reset_time > now {
                shortest_wait = shortest_wait.min(Duration::from_secs(
                    (reset_time - now).num_seconds().max(0) as u64,
                ));
            }
        }

        // Check tokens reset time
        if let Some(reset_time) = self.tokens_reset {
            if reset_time > now {
                shortest_wait = shortest_wait.min(Duration::from_secs(
                    (reset_time - now).num_seconds().max(0) as u64,
                ));
            }
        }

        // Add a small buffer to avoid hitting the limit exactly at reset time
        shortest_wait + Duration::from_secs(1)
    }

    /// Log current rate limit status
    fn log_status(&self) {
        debug!(
            "Rate limits - Requests: {}/{} (reset: {}), Tokens: {}/{} (reset: {})",
            self.requests_remaining
                .map_or("?".to_string(), |r| r.to_string()),
            self.requests_limit
                .map_or("?".to_string(), |l| l.to_string()),
            self.requests_reset
                .map_or("unknown".to_string(), |r| r.to_string()),
            self.tokens_remaining
                .map_or("?".to_string(), |r| r.to_string()),
            self.tokens_limit.map_or("?".to_string(), |l| l.to_string()),
            self.tokens_reset
                .map_or("unknown".to_string(), |r| r.to_string()),
        );
    }
}

/// Anthropic-specific request structure
#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    messages: Vec<Message>,
    max_tokens: usize,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum StreamEvent {
    #[serde(rename = "message_start")]
    MessageStart { message: MessageStart },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: usize,
        content_block: StreamContentBlock,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { index: usize, delta: ContentDelta },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: usize },
    #[serde(rename = "message_delta")]
    MessageDelta,
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
}

#[derive(Debug, Deserialize)]
struct MessageStart {
    id: String,
    #[serde(rename = "type")]
    message_type: String,
    role: String,
    model: String,
}

#[derive(Debug, Deserialize)]
struct StreamContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: Option<String>,
    // Fields for tool use blocks
    id: Option<String>,
    name: Option<String>,
    input: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ContentDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
}

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
            base_url: "https://api.anthropic.com/v1/messages".to_string(),
            model,
        }
    }

    async fn send_with_retry(
        &self,
        request: &AnthropicRequest,
        streaming_callback: Option<StreamingCallback>,
        max_retries: u32,
    ) -> Result<LLMResponse> {
        let mut attempts = 0;

        loop {
            match self
                .try_send_request(request, streaming_callback.clone())
                .await
            {
                Ok((response, rate_limits)) => {
                    // Log rate limit status on successful response
                    rate_limits.log_status();
                    return Ok(response);
                }
                Err(e) => {
                    // Extract rate limit info if available in the error context
                    let rate_limits = e
                        .downcast_ref::<ApiErrorContext<AnthropicRateLimitInfo>>()
                        .and_then(|ctx| ctx.rate_limits.as_ref());

                    match e.downcast_ref::<ApiError>() {
                        Some(ApiError::RateLimit(_)) => {
                            if let Some(rate_limits) = rate_limits {
                                if attempts < max_retries {
                                    attempts += 1;
                                    let delay = rate_limits.get_retry_delay();
                                    warn!(
                                            "Rate limit hit (attempt {}/{}), waiting {} seconds before retry",
                                            attempts,
                                            max_retries,
                                            delay.as_secs()
                                        );
                                    sleep(delay).await;
                                    continue;
                                }
                            } else {
                                // Fallback if no rate limit info available
                                if attempts < max_retries {
                                    attempts += 1;
                                    let delay = Duration::from_secs(2u64.pow(attempts - 1));
                                    warn!(
                                            "Rate limit hit but no timing info available (attempt {}/{}), using exponential backoff: {} seconds",
                                            attempts,
                                            max_retries,
                                            delay.as_secs()
                                        );
                                    sleep(delay).await;
                                    continue;
                                }
                            }
                        }
                        Some(ApiError::ServiceError(_)) => {
                            if attempts < max_retries {
                                attempts += 1;
                                let delay = Duration::from_secs(2u64.pow(attempts - 1));
                                warn!(
                                    "Service error (attempt {}/{}), retrying in {} seconds",
                                    attempts,
                                    max_retries,
                                    delay.as_secs()
                                );
                                sleep(delay).await;
                                continue;
                            }
                        }
                        Some(ApiError::NetworkError(_)) => {
                            if attempts < max_retries {
                                attempts += 1;
                                let delay = Duration::from_secs(2u64.pow(attempts - 1));
                                warn!(
                                    "Network error (attempt {}/{}), retrying in {} seconds",
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
        request: &AnthropicRequest,
        streaming_callback: Option<StreamingCallback>,
    ) -> Result<(LLMResponse, AnthropicRateLimitInfo)> {
        let accept_value = if let Some(_) = streaming_callback {
            "text/event-stream"
        } else {
            "application/json"
        };

        let mut response = self
            .client
            .post(&self.base_url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("accept", accept_value)
            .json(request)
            .send()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        // Log raw headers for debugging
        debug!("Response headers: {:?}", response.headers());

        // Extract rate limit information from response headers
        let rate_limits = AnthropicRateLimitInfo::from_response(&response);

        // Log parsed rate limits
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
                ApiError::Unknown(format!("Status {}: {}", status, response_text))
            };

            // Wrap the error with rate limit context
            return Err(ApiErrorContext {
                error,
                rate_limits: Some(rate_limits),
            }
            .into());
        }

        if let Some(callback) = streaming_callback {
            let mut blocks: Vec<ContentBlock> = Vec::new();
            let mut current_block_index: Option<usize> = None;
            let mut current_content = String::new();
            let mut line_buffer = String::new();

            fn process_chunk(
                chunk: &[u8],
                line_buffer: &mut String,
                blocks: &mut Vec<ContentBlock>,
                current_block_index: &mut Option<usize>,
                current_content: &mut String,
                callback: StreamingCallback,
            ) -> Result<()> {
                let chunk_str = str::from_utf8(chunk)?;

                for c in chunk_str.chars() {
                    if c == '\n' {
                        if !line_buffer.is_empty() {
                            process_sse_line(
                                line_buffer,
                                blocks,
                                current_block_index,
                                current_content,
                                callback,
                            )?;
                            line_buffer.clear();
                        }
                    } else {
                        line_buffer.push(c);
                    }
                }
                Ok(())
            }

            fn process_sse_line(
                line: &str,
                blocks: &mut Vec<ContentBlock>,
                current_block_index: &mut Option<usize>,
                current_content: &mut String,
                callback: StreamingCallback,
            ) -> Result<()> {
                if let Some(data) = line.strip_prefix("data: ") {
                    if let Ok(event) = serde_json::from_str::<StreamEvent>(data) {
                        match event {
                            StreamEvent::ContentBlockStart {
                                index,
                                content_block,
                            } => {
                                *current_block_index = Some(index);
                                if blocks.len() <= index {
                                    // Create the right content block type based on the received type
                                    let block = match content_block.block_type.as_str() {
                                        "text" => ContentBlock::Text {
                                            text: content_block.text.unwrap_or_default(),
                                        },
                                        "tool_use" => ContentBlock::ToolUse {
                                            id: content_block.id.unwrap_or_default(),
                                            name: content_block.name.unwrap_or_default(),
                                            input: serde_json::Value::Null,
                                        },
                                        _ => ContentBlock::Text {
                                            // Fallback for unknown types
                                            text: String::new(),
                                        },
                                    };
                                    blocks.push(block);
                                }
                                current_content.clear();
                            }
                            StreamEvent::ContentBlockDelta { index: _, delta } => {
                                if let Some(_) = current_block_index {
                                    match &delta {
                                        ContentDelta::TextDelta { text: delta_text } => {
                                            callback(delta_text)?;
                                            current_content.push_str(delta_text);
                                        }
                                        ContentDelta::InputJsonDelta { partial_json } => {
                                            // Accumulate JSON parts as string
                                            current_content.push_str(partial_json);
                                        }
                                        _ => {} // Ignore mismatched block/delta types
                                    }
                                }
                            }
                            StreamEvent::ContentBlockStop { index } => {
                                if let Some(block) = blocks.get_mut(index) {
                                    match block {
                                        ContentBlock::Text { text } => {
                                            *text = current_content.clone();
                                        }
                                        ContentBlock::ToolUse { input, .. } => {
                                            if let Ok(json) = serde_json::from_str(current_content)
                                            {
                                                *input = json;
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                *current_block_index = None;
                            }
                            _ => {}
                        }
                    }
                }
                Ok(())
            }

            while let Some(chunk) = response.chunk().await? {
                process_chunk(
                    &chunk,
                    &mut line_buffer,
                    &mut blocks,
                    &mut current_block_index,
                    &mut current_content,
                    callback,
                )?;
            }

            // Process any remaining data in the buffer
            if !line_buffer.is_empty() {
                process_sse_line(
                    &line_buffer,
                    &mut blocks,
                    &mut current_block_index,
                    &mut current_content,
                    callback,
                )?;
            }

            Ok((LLMResponse { content: blocks }, rate_limits))
        } else {
            let response_text = response
                .text()
                .await
                .map_err(|e| ApiError::NetworkError(e.to_string()))?;

            let llm_response = serde_json::from_str(&response_text)
                .map_err(|e| ApiError::Unknown(format!("Failed to parse response: {}", e)))?;

            Ok((llm_response, rate_limits))
        }
    }
}

#[async_trait]
impl LLMProvider for AnthropicClient {
    async fn send_message(
        &self,
        request: LLMRequest,
        streaming_callback: Option<StreamingCallback>,
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

        self.send_with_retry(&anthropic_request, streaming_callback, 3)
            .await
    }
}
