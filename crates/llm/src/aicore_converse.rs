use crate::llm::{
    recording::APIRecorder, types::*, utils, ApiError, LLMProvider, RateLimitHandler,
    StreamingCallback, StreamingChunk,
};
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};
use std::str::{self};
use std::sync::Arc;
use std::time::Duration;
use tracing::debug;

use super::auth::TokenManager;

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

/// AWS Bedrock Converse request structure for all models
#[derive(Debug, Serialize)]
struct ConverseRequest {
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<Vec<SystemBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "inferenceConfig")]
    inference_config: Option<InferenceConfiguration>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "additionalModelRequestFields"
    )]
    additional_model_request_fields: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "toolConfig")]
    tool_config: Option<ToolConfiguration>,
}

#[derive(Debug, Serialize)]
struct InferenceConfiguration {
    max_tokens: usize,
    temperature: f32,
}

#[derive(Debug, Serialize)]
struct ToolConfiguration {
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
}
/// Response structure for AWS Bedrock API responses
#[derive(Debug, Deserialize)]
struct ConverseResponse {
    output: ConverseOutput,
    #[serde(default)]
    #[allow(dead_code)]
    stop_reason: String,
    usage: TokenUsage,
}

#[derive(Debug, Deserialize)]
struct ConverseOutput {
    message: Message,
}

/// Usage information from AiCore Converse API
#[derive(Debug, Deserialize)]
struct TokenUsage {
    #[serde(default, rename = "inputTokens")]
    input_tokens: u32,
    #[serde(default, rename = "outputTokens")]
    output_tokens: u32,
    #[serde(default, rename = "totalTokens")]
    total_tokens: u32,
    #[serde(default, rename = "cacheCreationInputTokens")]
    cache_creation_input_tokens: u32,
    #[serde(default, rename = "cacheReadInputTokens")]
    cache_read_input_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct StreamEventCommon {
    #[serde(rename = "contentBlockIndex")]
    index: usize,
}

// Structure to parse SSE events from the API
#[derive(Debug, Deserialize)]
struct StreamEvent {
    #[serde(default)]
    #[serde(rename = "messageStart")]
    message_start: Option<MessageStartEvent>,

    #[serde(default)]
    #[serde(rename = "contentBlockStart")]
    content_block_start: Option<ContentBlockStartEvent>,

    #[serde(default)]
    #[serde(rename = "contentBlockDelta")]
    content_block_delta: Option<ContentBlockDeltaEvent>,

    #[serde(default)]
    #[serde(rename = "contentBlockStop")]
    content_block_stop: Option<ContentBlockStopEvent>,

    #[serde(default)]
    #[serde(rename = "messageStop")]
    message_stop: Option<MessageStopEvent>,

    #[serde(default)]
    metadata: Option<MetadataEvent>,

    #[serde(default)]
    ping: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct MessageStartEvent {
    role: String,
}

#[derive(Debug, Deserialize)]
struct ContentBlockStartEvent {
    start: StreamContentBlockStart,
    #[serde(rename = "contentBlockIndex")]
    index: usize,
}

#[derive(Debug, Deserialize)]
struct ContentBlockDeltaEvent {
    delta: serde_json::Value,
    #[serde(rename = "contentBlockIndex")]
    index: usize,
}

#[derive(Debug, Deserialize)]
struct ContentBlockStopEvent {
    #[serde(rename = "contentBlockIndex")]
    index: usize,
}

#[derive(Debug, Deserialize)]
struct MessageStopEvent {
    #[serde(rename = "stopReason")]
    stop_reason: String,
    #[serde(default)]
    #[serde(rename = "additionalModelResponseFields")]
    additional_model_response_fields: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct MetadataEvent {
    usage: Option<TokenUsage>,
    metrics: Option<ConverseMetrics>,
    trace: Option<ConverseTrace>,
}

#[derive(Debug, Deserialize)]
struct ConverseMetrics {
    #[serde(rename = "latencyMs")]
    latency_ms: u64,
}

#[derive(Debug, Deserialize)]
struct ConverseTrace {
    guardrail: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct StreamContentBlockStart {
    #[serde(rename = "type")]
    block_type: String,
    // Fields for text blocks
    text: Option<String>,
    // Fields for thinking blocks
    thinking: Option<String>,
    signature: Option<String>,
    // Fields for tool use blocks
    id: Option<String>,
    name: Option<String>,
    input: Option<String>,
}

// We use serde_json::Value directly for content deltas since
// the response structure can vary and may be transformed by proxies

pub struct AiCoreClient {
    token_manager: Arc<TokenManager>,
    client: Client,
    base_url: String,
    recorder: Option<APIRecorder>,
}

impl AiCoreClient {
    pub fn new(token_manager: Arc<TokenManager>, base_url: String) -> Self {
        Self {
            token_manager,
            client: Client::new(),
            base_url,
            recorder: None,
        }
    }

    /// Create a new client with recording capability
    pub fn new_with_recorder<P: AsRef<std::path::Path>>(
        token_manager: Arc<TokenManager>,
        base_url: String,
        recording_path: P,
    ) -> Self {
        Self {
            token_manager,
            client: Client::new(),
            base_url,
            recorder: Some(APIRecorder::new(recording_path)),
        }
    }

    fn get_url(&self, streaming: bool) -> String {
        if streaming {
            format!("{}/converse-stream", self.base_url)
        } else {
            format!("{}/converse", self.base_url)
        }
    }

    async fn send_with_retry(
        &mut self,
        request: &ConverseRequest,
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
                        streaming_callback,
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
        &mut self,
        request: &ConverseRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<(LLMResponse, AnthropicRateLimitInfo)> {
        let token = self.token_manager.get_valid_token().await?;

        // Start recording before HTTP request to capture real latency
        if let Some(recorder) = &self.recorder {
            let request_json = serde_json::to_value(request)?;
            recorder.start_recording(request_json)?;
        }

        let request_builder = self
            .client
            .post(&self.get_url(streaming_callback.is_some()))
            .header("AI-Resource-Group", "default")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", token));

        let response = request_builder
            .json(&request)
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
            let mut usage = TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            };

            fn process_chunk(
                chunk: &[u8],
                line_buffer: &mut String,
                blocks: &mut Vec<ContentBlock>,
                usage: &mut TokenUsage,
                current_content: &mut String,
                callback: &StreamingCallback,
                recorder: &Option<APIRecorder>,
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
                                recorder,
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
                usage: &mut TokenUsage,
                current_content: &mut String,
                callback: &StreamingCallback,
                recorder: &Option<APIRecorder>,
            ) -> Result<()> {
                if let Some(data) = line.strip_prefix("data: ") {
                    println!("SSE line: {}", data);
                    match serde_json::from_str::<StreamEvent>(data) {
                        Ok(event) => {
                            // Record the chunk if recorder is available
                            if let Some(recorder) = &recorder {
                                recorder.record_chunk(data)?;
                            }

                            // Process based on which event type was received
                            if let Some(content_start) = &event.content_block_start {
                                // Check the index matches expected
                                let index = content_start.index;
                                if index != blocks.len() {
                                    return Err(anyhow::anyhow!(
                                        "Start index {} does not match expected block {}",
                                        index,
                                        blocks.len()
                                    ));
                                }

                                current_content.clear();
                                let block = match content_start.start.block_type.as_str() {
                                    "thinking" => {
                                        if let Some(thinking) = &content_start.start.thinking {
                                            current_content.push_str(thinking);
                                        }
                                        ContentBlock::Thinking {
                                            thinking: current_content.clone(),
                                            signature: content_start
                                                .start
                                                .signature
                                                .clone()
                                                .unwrap_or_default(),
                                        }
                                    }
                                    "text" => {
                                        if let Some(text) = &content_start.start.text {
                                            current_content.push_str(text);
                                        }
                                        ContentBlock::Text {
                                            text: current_content.clone(),
                                        }
                                    }
                                    "tool_use" => {
                                        if let Some(input) = &content_start.start.input {
                                            current_content.push_str(input);
                                        }
                                        ContentBlock::ToolUse {
                                            id: content_start.start.id.clone().unwrap_or_default(),
                                            name: content_start
                                                .start
                                                .name
                                                .clone()
                                                .unwrap_or_default(),
                                            input: serde_json::Value::Null,
                                        }
                                    }
                                    _ => ContentBlock::Text {
                                        text: String::new(),
                                    },
                                };
                                blocks.push(block);
                            } else if let Some(content_delta) = &event.content_block_delta {
                                let index = content_delta.index;

                                // Check if we have any blocks at all
                                if blocks.is_empty() {
                                    return Err(anyhow::anyhow!(
                                        "Received Delta but no blocks exist"
                                    ));
                                }

                                if index != blocks.len() - 1 {
                                    return Err(anyhow::anyhow!(
                                        "Delta index {} does not match current block {}",
                                        index,
                                        blocks.len() - 1
                                    ));
                                }

                                // Try to extract the text from the delta
                                if let Some(text_obj) = content_delta.delta.get("text") {
                                    if let Some(text) = text_obj.as_str() {
                                        callback(&StreamingChunk::Text(text.to_string()))?;
                                        current_content.push_str(text);
                                    }
                                } else if content_delta.delta.get("SDK_UNKNOWN_MEMBER").is_some() {
                                    // This is the reasoningContent delta
                                    if let Some(reasoning) =
                                        content_delta.delta["SDK_UNKNOWN_MEMBER"].get("name")
                                    {
                                        if reasoning.as_str() == Some("reasoningContent") {
                                            // Treat this as thinking content for now
                                            // In a real implementation, we'd extract the text, but it's not available in the payload
                                            callback(&StreamingChunk::Thinking(
                                                "Thinking...".to_string(),
                                            ))?;
                                        }
                                    }
                                } else if let Some(partial_json) =
                                    content_delta.delta.get("partialJson")
                                {
                                    if let Some(partial_json_str) = partial_json.as_str() {
                                        current_content.push_str(partial_json_str);
                                    }
                                }
                            } else if let Some(content_stop) = &event.content_block_stop {
                                let index = content_stop.index;

                                // Check if we have any blocks at all
                                if blocks.is_empty() {
                                    return Err(anyhow::anyhow!(
                                        "Received Stop but no blocks exist"
                                    ));
                                }

                                if index != blocks.len() - 1 {
                                    return Err(anyhow::anyhow!(
                                        "Stop index {} does not match current block {}",
                                        index,
                                        blocks.len() - 1
                                    ));
                                }

                                match blocks.last_mut().unwrap() {
                                    ContentBlock::Text { text } => {
                                        *text = current_content.clone();
                                    }
                                    ContentBlock::ToolUse { input, .. } => {
                                        if let Ok(json) = serde_json::from_str(&current_content) {
                                            *input = json;
                                        }
                                    }
                                    _ => {}
                                }
                            } else if let Some(metadata) = &event.metadata {
                                if let Some(meta_usage) = &metadata.usage {
                                    usage.input_tokens = meta_usage.input_tokens;
                                    usage.output_tokens = meta_usage.output_tokens;
                                    usage.cache_creation_input_tokens =
                                        meta_usage.cache_creation_input_tokens;
                                    usage.cache_read_input_tokens =
                                        meta_usage.cache_read_input_tokens;
                                }
                            }
                        }
                        Err(e) => {
                            println!("[ERROR] Failed to parse event: {}", e);
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
                    &self.recorder,
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
                    &self.recorder,
                )?;
            }

            // End recording if a recorder is available
            if let Some(recorder) = &self.recorder {
                recorder.end_recording()?;
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
                    rate_limit_info: None,
                },
                rate_limits,
            ))
        } else {
            let response_text = response
                .text()
                .await
                .map_err(|e| ApiError::NetworkError(e.to_string()))?;

            let converse_response: ConverseResponse = serde_json::from_str(&response_text)
                .map_err(|e| ApiError::Unknown(format!("Failed to parse response: {}", e)))?;

            let content = match converse_response.output.message.content {
                MessageContent::Text(text) => vec![ContentBlock::Text { text }],
                MessageContent::Structured(blocks) => blocks,
            };

            let llm_response = LLMResponse {
                content,
                usage: Usage {
                    input_tokens: converse_response.usage.input_tokens,
                    output_tokens: converse_response.usage.output_tokens,
                    cache_creation_input_tokens: converse_response
                        .usage
                        .cache_creation_input_tokens,
                    cache_read_input_tokens: converse_response.usage.cache_read_input_tokens,
                },
                rate_limit_info: None,
            };

            Ok((llm_response, rate_limits))
        }
    }
}

#[async_trait]
impl LLMProvider for AiCoreClient {
    async fn send_message(
        &mut self,
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

        // Always enable thinking mode and max tokens for large models
        let thinking = Some(ThinkingConfiguration {
            thinking_type: "enabled".to_string(),
            budget_tokens: 4000,
        });
        let max_tokens = 128000;

        // In der send_message-Methode
        let tool_config = if has_tools {
            Some(ToolConfiguration { tools, tool_choice })
        } else {
            None
        };

        let inference_config = Some(InferenceConfiguration {
            max_tokens,
            temperature: 1.0,
        });

        let messages = request
            .messages
            .into_iter()
            .map(|mut message| {
                if let MessageContent::Text(text) = message.content {
                    message.content = MessageContent::Structured(vec![ContentBlock::Text { text }]);
                }
                message
            })
            .collect();

        let converse_request = ConverseRequest {
            messages,
            system,
            inference_config,
            tool_config,
            additional_model_request_fields: Some(serde_json::json!({
            //                "streaming": streaming_callback.is_some(),
                            "thinking": thinking
                        })),
        };

        self.send_with_retry(&converse_request, streaming_callback, 3)
            .await
    }
}
