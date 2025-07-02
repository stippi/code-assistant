use crate::{
    recording::APIRecorder, types::*, utils, ApiError, LLMProvider, RateLimitHandler,
    StreamingCallback, StreamingChunk,
};
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::str::{self};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, warn};

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
#[derive(Debug, Default)]
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

/// Bedrock Invoke message structure
#[derive(Debug, Serialize)]
struct AiCoreMessage {
    role: String,
    content: Vec<AiCoreContentBlock>,
}

/// Bedrock Invoke content block structure
#[derive(Debug, Serialize)]
struct AiCoreContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(flatten)]
    content: AiCoreBlockContent,
}

/// Content variants for Bedrock Invoke content blocks
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum AiCoreBlockContent {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<Vec<AiCoreToolResultContent>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

/// Tool result content for Bedrock Invoke
#[derive(Debug, Serialize)]
struct AiCoreToolResultContent {
    #[serde(rename = "type")]
    content_type: String,
    #[serde(flatten)]
    data: AiCoreToolResultData,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum AiCoreToolResultData {
    Text { text: String },
}

/// Bedrock Invoke-specific request structure
#[derive(Debug, Serialize)]
struct AnthropicRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfiguration>,
    messages: Vec<AiCoreMessage>,
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
struct StreamErrorDetails {
    #[serde(rename = "type")]
    error_type: String,
    message: String,
    #[allow(dead_code)]
    details: Option<serde_json::Value>,
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
    #[serde(rename = "error")]
    Error { error: StreamErrorDetails },
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
    // Fields for text blocks
    text: Option<String>,
    // Fields for thinking blocks
    thinking: Option<String>,
    signature: Option<String>,
    // Fields for redacted_thinking blocks
    data: Option<String>,
    // Fields for tool use blocks
    id: Option<String>,
    name: Option<String>,
    input: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ContentDelta {
    #[serde(rename = "thinking_delta")]
    Thinking { thinking: String },
    #[serde(rename = "signature_delta")]
    Signature { signature: String },
    #[serde(rename = "text_delta")]
    Text { text: String },
    #[serde(rename = "input_json_delta")]
    InputJson { partial_json: String },
}

/// Convert LLM messages to Bedrock Invoke format (removes internal fields like request_id)
fn convert_messages_to_aicore(messages: Vec<Message>) -> Vec<AiCoreMessage> {
    messages
        .into_iter()
        .map(|msg| AiCoreMessage {
            role: match msg.role {
                MessageRole::User => "user".to_string(),
                MessageRole::Assistant => "assistant".to_string(),
            },
            content: convert_content_to_aicore(msg.content),
        })
        .collect()
}

/// Convert LLM message content to Bedrock Invoke format
fn convert_content_to_aicore(content: MessageContent) -> Vec<AiCoreContentBlock> {
    match content {
        MessageContent::Text(text) => {
            vec![AiCoreContentBlock {
                block_type: "text".to_string(),
                content: AiCoreBlockContent::Text { text },
            }]
        }
        MessageContent::Structured(blocks) => blocks
            .into_iter()
            .map(|block| convert_content_block_to_aicore(block))
            .collect(),
    }
}

/// Convert a single content block to Bedrock Invoke format
fn convert_content_block_to_aicore(block: ContentBlock) -> AiCoreContentBlock {
    match block {
        ContentBlock::Text { text } => AiCoreContentBlock {
            block_type: "text".to_string(),
            content: AiCoreBlockContent::Text { text },
        },
        ContentBlock::ToolUse { id, name, input } => AiCoreContentBlock {
            block_type: "tool_use".to_string(),
            content: AiCoreBlockContent::ToolUse { id, name, input },
        },
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let tool_content = Some(vec![AiCoreToolResultContent {
                content_type: "text".to_string(),
                data: AiCoreToolResultData::Text { text: content },
            }]);
            AiCoreContentBlock {
                block_type: "tool_result".to_string(),
                content: AiCoreBlockContent::ToolResult {
                    tool_use_id,
                    content: tool_content,
                    is_error,
                },
            }
        }
        ContentBlock::Thinking {
            thinking,
            signature,
        } => AiCoreContentBlock {
            block_type: "thinking".to_string(),
            content: AiCoreBlockContent::Thinking {
                thinking,
                signature,
            },
        },
        ContentBlock::RedactedThinking { data } => AiCoreContentBlock {
            block_type: "redacted_thinking".to_string(),
            content: AiCoreBlockContent::RedactedThinking { data },
        },
    }
}

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
            format!("{}/invoke-with-response-stream", self.base_url)
        } else {
            format!("{}/invoke", self.base_url)
        }
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
        &self,
        request: &AnthropicRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<(LLMResponse, AnthropicRateLimitInfo)> {
        let token = self.token_manager.get_valid_token().await?;

        let request_builder = self
            .client
            .post(self.get_url(streaming_callback.is_some()))
            .header("AI-Resource-Group", "default")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", token))
            .header("anthropic-beta", "output-128k-2025-02-19");

        let mut request = serde_json::to_value(request)?;
        if let Value::Object(ref mut map) = request {
            map.remove("stream"); // Remove stream after we redirect to /invoke-with-response-stream
            map.insert(
                "anthropic_version".to_string(),
                Value::String("bedrock-2023-05-31".to_string()),
            );
        }

        // Start recording before HTTP request to capture real latency
        if let Some(recorder) = &self.recorder {
            recorder.start_recording(request.clone())?;
        }

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
            debug!("Starting streaming response processing");
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
                usage: &mut AnthropicUsage,
                current_content: &mut String,
                callback: &StreamingCallback,
                recorder: &Option<APIRecorder>,
            ) -> Result<()> {
                if let Some(data) = line.strip_prefix("data: ") {
                    // Record the chunk if recorder is available
                    if let Some(recorder) = &recorder {
                        recorder.record_chunk(data)?;
                    }
                    if let Ok(event) = serde_json::from_str::<StreamEvent>(data) {
                        // Handle error events immediately
                        if let StreamEvent::Error { error } = &event {
                            let error_msg = format!("{}: {}", error.error_type, error.message);
                            return match error.error_type.as_str() {
                                "overloaded_error" => Err(ApiErrorContext {
                                    error: ApiError::Overloaded(error_msg),
                                    rate_limits: Some(AnthropicRateLimitInfo::default()),
                                }
                                .into()),
                                _ => Err(anyhow::anyhow!("Stream error: {}", error_msg)),
                            };
                        }

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
                                    "thinking" => {
                                        if let Some(thinking) = content_block.thinking {
                                            current_content.push_str(&thinking);
                                        }
                                        ContentBlock::Thinking {
                                            thinking: current_content.clone(),
                                            signature: content_block.signature.unwrap_or_default(),
                                        }
                                    }
                                    "redacted_thinking" => {
                                        if let Some(data) = content_block.data {
                                            current_content.push_str(&data);
                                        }
                                        ContentBlock::RedactedThinking {
                                            data: current_content.clone(),
                                        }
                                    }
                                    "text" => {
                                        if let Some(text) = content_block.text {
                                            current_content.push_str(&text);
                                        }
                                        ContentBlock::Text {
                                            text: current_content.clone(),
                                        }
                                    }
                                    "tool_use" => {
                                        // Handle input as JSON value directly
                                        let input_json = if let Some(input) = &content_block.input {
                                            input.clone()
                                        } else {
                                            serde_json::Value::Null
                                        };

                                        let tool_id = content_block.id.unwrap_or_default();
                                        let tool_name = content_block.name.unwrap_or_default();

                                        debug!(
                                            "Creating ToolUse block with id={:?}, name={:?}",
                                            tool_id, tool_name
                                        );

                                        ContentBlock::ToolUse {
                                            id: tool_id,
                                            name: tool_name,
                                            input: input_json,
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
                                    ContentDelta::Thinking {
                                        thinking: delta_text,
                                    } => {
                                        callback(&StreamingChunk::Thinking(delta_text.clone()))?;
                                        current_content.push_str(delta_text);
                                    }
                                    ContentDelta::Signature {
                                        signature: signature_delta,
                                    } => {
                                        // Update the signature in the last block if it's a thinking block
                                        if let ContentBlock::Thinking { signature, .. } =
                                            blocks.last_mut().unwrap()
                                        {
                                            *signature = signature_delta.clone();
                                        }
                                    }
                                    ContentDelta::Text { text: delta_text } => {
                                        callback(&StreamingChunk::Text(delta_text.clone()))?;
                                        current_content.push_str(delta_text);
                                    }
                                    ContentDelta::InputJson { partial_json } => {
                                        let (tool_name, tool_id) =
                                            blocks.last().map_or((None, None), |block| {
                                                if let ContentBlock::ToolUse { name, id, .. } =
                                                    block
                                                {
                                                    (Some(name.clone()), Some(id.clone()))
                                                } else {
                                                    warn!("Last block is not a ToolUse type!");
                                                    (None, None)
                                                }
                                            });

                                        callback(&StreamingChunk::InputJson {
                                            content: partial_json.clone(),
                                            tool_name,
                                            tool_id,
                                        })?;

                                        current_content.push_str(partial_json);
                                    }
                                }
                            }
                            StreamEvent::ContentBlockStop { .. } => {
                                match blocks.last_mut().unwrap() {
                                    ContentBlock::Thinking { thinking, .. } => {
                                        *thinking = current_content.clone();
                                    }
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
                    } else {
                        return Err(anyhow::anyhow!("Failed to parse stream event:\n{}", line));
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
            debug!("Processing non-streaming response");
            let response_text = response.text().await.map_err(|e| {
                error!("Failed to read response text: {}", e);
                ApiError::NetworkError(e.to_string())
            })?;

            let anthropic_response: AnthropicResponse = serde_json::from_str(&response_text)
                .map_err(|e| {
                    error!("Failed to parse response JSON: {}", e);
                    ApiError::Unknown(format!("Failed to parse response: {}", e))
                })?;

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
            cache_control: None,
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
            tools
                .into_iter()
                .map(|tool| {
                    serde_json::json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.parameters
                    })
                })
                .collect::<Vec<serde_json::Value>>()
        });

        // Convert messages to Bedrock Invoke format (remove internal fields)
        let aicore_messages = convert_messages_to_aicore(request.messages);

        let anthropic_request = AnthropicRequest {
            thinking: None,
            messages: aicore_messages,
            max_tokens: 8192,
            temperature: 1.0,
            system,
            stream: streaming_callback.map(|_| true),
            tool_choice,
            tools,
        };

        self.send_with_retry(&anthropic_request, streaming_callback, 3)
            .await
    }
}
