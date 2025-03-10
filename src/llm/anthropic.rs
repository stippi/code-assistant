use crate::llm::{types::*, utils, ApiError, LLMProvider, RateLimitHandler, StreamingCallback};
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};
use std::str::{self};
use std::time::Duration;
use tracing::debug;

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

/// Cache control settings for Anthropic API request
#[derive(Debug, Serialize)]
struct CacheControl {
    #[serde(rename = "type")]
    cache_type: String,
}

/// System content block with optional cache control
#[derive(Debug, Serialize)]
struct SystemBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

#[derive(Debug, Serialize)]
struct ThinkingConfiguration {
    #[serde(rename = "type")]
    thinking_type: String,
    budget_tokens: usize,
}

/// Anthropic-specific request structure
#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfiguration>,
    messages: Vec<Message>,
    max_tokens: usize,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<Vec<SystemBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

/// Response structure for Anthropic API responses
#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<ContentBlock>,
    #[serde(default)]
    #[allow(dead_code)]
    id: String,
    #[serde(default)]
    #[allow(dead_code)]
    model: String,
    #[serde(default)]
    #[allow(dead_code)]
    role: String,
    #[serde(rename = "type", default)]
    #[allow(dead_code)]
    response_type: String,
    #[serde(default)]
    #[allow(dead_code)]
    stop_reason: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    stop_sequence: Option<String>,
    usage: AnthropicUsage,
}

/// Usage information from Anthropic API
#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct StreamEventCommon {
    index: usize,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum StreamEvent {
    #[allow(dead_code)]
    #[serde(rename = "message_start")]
    MessageStart { message: MessageStart },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        #[serde(flatten)]
        common: StreamEventCommon,
        content_block: StreamContentBlock,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta {
        #[serde(flatten)]
        common: StreamEventCommon,
        delta: ContentDelta,
    },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop {
        #[serde(flatten)]
        common: StreamEventCommon,
    },
    #[serde(rename = "message_delta")]
    MessageDelta { usage: AnthropicUsage },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
}

#[derive(Debug, Deserialize)]
struct MessageStart {
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    #[serde(rename = "type")]
    message_type: String,
    #[allow(dead_code)]
    role: String,
    #[allow(dead_code)]
    model: String,
    usage: AnthropicUsage,
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

    async fn send_with_retry(
        &self,
        request: &AnthropicRequest,
        streaming_callback: Option<&StreamingCallback>,
        max_retries: u32,
    ) -> Result<LLMResponse> {
        let mut attempts = 0;

        loop {
            match self.try_send_request(request, streaming_callback).await {
                Ok((response, rate_limits)) => {
                    // Log rate limit status on successful response
                    rate_limits.log_status();
                    return Ok(response);
                }
                Err(e) => {
                    if utils::handle_retryable_error::<AnthropicRateLimitInfo>(
                        &e,
                        attempts,
                        max_retries,
                    )
                    .await
                    {
                        attempts += 1;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
    }

    async fn try_send_request(
        &self,
        request: &AnthropicRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<(LLMResponse, AnthropicRateLimitInfo)> {
        let accept_value = if let Some(_) = streaming_callback {
            "text/event-stream"
        } else {
            "application/json"
        };

        let mut request_builder = self
            .client
            .post(&self.get_url())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("accept", accept_value);

        if self.model.starts_with("claude-3-7-sonnet") {
            request_builder = request_builder.header("anthropic-beta", "output-128k-2025-02-19");
        }

        let response = request_builder
            .json(request)
            .send()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        // Log raw headers for debugging
        debug!("Response headers: {:?}", response.headers());

        let mut response = utils::check_response_error::<AnthropicRateLimitInfo>(response).await?;
        let rate_limits = AnthropicRateLimitInfo::from_response(&response);

        // Log parsed rate limits
        debug!("Parsed rate limits: {:?}", rate_limits);

        if let Some(callback) = streaming_callback {
            let mut blocks: Vec<ContentBlock> = Vec::new();
            let mut current_content = String::new();
            let mut line_buffer = String::new();
            let mut usage = AnthropicUsage {
                input_tokens: 0,
                output_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            };

            fn process_chunk(
                chunk: &[u8],
                line_buffer: &mut String,
                blocks: &mut Vec<ContentBlock>,
                usage: &mut AnthropicUsage,
                current_content: &mut String,
                callback: &StreamingCallback,
            ) -> Result<()> {
                let chunk_str = str::from_utf8(chunk)?;

                for c in chunk_str.chars() {
                    if c == '\n' {
                        if !line_buffer.is_empty() {
                            process_sse_line(
                                line_buffer,
                                blocks,
                                usage,
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
                usage: &mut AnthropicUsage,
                current_content: &mut String,
                callback: &StreamingCallback,
            ) -> Result<()> {
                if let Some(data) = line.strip_prefix("data: ") {
                    if let Ok(event) = serde_json::from_str::<StreamEvent>(data) {
                        // Extract and check index for relevant events
                        match &event {
                            StreamEvent::ContentBlockStart { common, .. } => {
                                if common.index != blocks.len() {
                                    return Err(anyhow::anyhow!(
                                        "Start index {} does not match expected block {}",
                                        common.index,
                                        blocks.len()
                                    ));
                                }
                            }
                            StreamEvent::ContentBlockDelta { common, .. }
                            | StreamEvent::ContentBlockStop { common } => {
                                // Check if we have any blocks at all
                                if blocks.is_empty() {
                                    return Err(anyhow::anyhow!(
                                        "Received Delta/Stop but no blocks exist"
                                    ));
                                }
                                if common.index != blocks.len() - 1 {
                                    return Err(anyhow::anyhow!(
                                        "Delta/Stop index {} does not match current block {}",
                                        common.index,
                                        blocks.len() - 1
                                    ));
                                }
                            }
                            StreamEvent::MessageStart { message } => {
                                usage.input_tokens = message.usage.input_tokens;
                                usage.output_tokens = message.usage.output_tokens;
                                usage.cache_creation_input_tokens =
                                    message.usage.cache_creation_input_tokens;
                                usage.cache_read_input_tokens =
                                    message.usage.cache_read_input_tokens;
                                return Ok(());
                            }
                            StreamEvent::MessageDelta { usage: delta_usage } => {
                                usage.output_tokens = delta_usage.output_tokens;
                                return Ok(());
                            }
                            _ => return Ok(()), // Early return for events without index
                        }

                        match event {
                            StreamEvent::ContentBlockStart { content_block, .. } => {
                                current_content.clear();
                                let block = match content_block.block_type.as_str() {
                                    "text" => {
                                        if let Some(text) = content_block.text {
                                            current_content.push_str(&text);
                                        }
                                        ContentBlock::Text {
                                            text: String::new(),
                                        }
                                    }
                                    "tool_use" => {
                                        if let Some(input) = content_block.input {
                                            current_content.push_str(&input);
                                        }
                                        ContentBlock::ToolUse {
                                            id: content_block.id.unwrap_or_default(),
                                            name: content_block.name.unwrap_or_default(),
                                            input: serde_json::Value::Null,
                                        }
                                    }
                                    _ => ContentBlock::Text {
                                        text: String::new(),
                                    },
                                };
                                blocks.push(block);
                            }
                            StreamEvent::ContentBlockDelta { delta, .. } => {
                                match &delta {
                                    ContentDelta::TextDelta { text: delta_text } => {
                                        callback(delta_text)?;
                                        current_content.push_str(delta_text);
                                    }
                                    ContentDelta::InputJsonDelta { partial_json } => {
                                        // Accumulate JSON parts as string
                                        current_content.push_str(partial_json);
                                    }
                                }
                            }
                            StreamEvent::ContentBlockStop { .. } => {
                                match blocks.last_mut().unwrap() {
                                    ContentBlock::Text { text } => {
                                        *text = current_content.clone();
                                    }
                                    ContentBlock::ToolUse { input, .. } => {
                                        if let Ok(json) = serde_json::from_str(current_content) {
                                            *input = json;
                                        }
                                    }
                                    _ => {}
                                }
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
                    &mut usage,
                    &mut current_content,
                    callback,
                )?;
            }

            // Process any remaining data in the buffer
            if !line_buffer.is_empty() {
                process_sse_line(
                    &line_buffer,
                    &mut blocks,
                    &mut usage,
                    &mut current_content,
                    callback,
                )?;
            }

            Ok((
                LLMResponse {
                    content: blocks,
                    usage: Usage {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        cache_creation_input_tokens: usage.cache_creation_input_tokens,
                        cache_read_input_tokens: usage.cache_read_input_tokens,
                    },
                },
                rate_limits,
            ))
        } else {
            let response_text = response
                .text()
                .await
                .map_err(|e| ApiError::NetworkError(e.to_string()))?;

            let anthropic_response: AnthropicResponse = serde_json::from_str(&response_text)
                .map_err(|e| ApiError::Unknown(format!("Failed to parse response: {}", e)))?;

            // Convert AnthropicResponse to LLMResponse
            let llm_response = LLMResponse {
                content: anthropic_response.content,
                usage: Usage {
                    input_tokens: anthropic_response.usage.input_tokens,
                    output_tokens: anthropic_response.usage.output_tokens,
                    cache_creation_input_tokens: anthropic_response
                        .usage
                        .cache_creation_input_tokens,
                    cache_read_input_tokens: anthropic_response.usage.cache_read_input_tokens,
                },
            };

            Ok((llm_response, rate_limits))
        }
    }
}

#[async_trait]
impl LLMProvider for AnthropicClient {
    async fn send_message(
        &self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        // Convert system prompt to system blocks with cache control
        let system = Some(vec![SystemBlock {
            block_type: "text".to_string(),
            text: request.system_prompt,
            // Add cache_control to the system prompt to utilize Anthropic's caching
            cache_control: Some(CacheControl {
                cache_type: "ephemeral".to_string(),
            }),
        }]);

        // Determine if we have tools and create tool_choice
        let has_tools = request.tools.is_some();
        let tool_choice = if has_tools {
            Some(serde_json::json!({
                "type": "any",
            }))
        } else {
            None
        };

        // Create tools array with cache control on the last tool if present
        let tools = request.tools.map(|tools| {
            let mut tools_json = tools
                .into_iter()
                .map(|tool| {
                    serde_json::json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.parameters
                    })
                })
                .collect::<Vec<serde_json::Value>>();

            // Add cache_control to the last tool if any exist
            if let Some(last_tool) = tools_json.last_mut() {
                if let Some(obj) = last_tool.as_object_mut() {
                    obj.insert(
                        "cache_control".to_string(),
                        serde_json::json!({"type": "ephemeral"}),
                    );
                }
            }

            tools_json
        });

        let thinking = if self.model.starts_with("claude-3-7-sonnet") {
            Some(ThinkingConfiguration {
                thinking_type: "enabled".to_string(),
                budget_tokens: 16000,
            })
        } else {
            None
        };

        let max_tokens = if self.model.starts_with("claude-3-7-sonnet") {
            128000
        } else {
            8192
        };

        let anthropic_request = AnthropicRequest {
            model: self.model.clone(),
            thinking,
            messages: request.messages,
            max_tokens,
            temperature: 0.7,
            system,
            stream: streaming_callback.map(|_| true),
            tool_choice,
            tools,
        };

        self.send_with_retry(&anthropic_request, streaming_callback, 3)
            .await
    }
}
