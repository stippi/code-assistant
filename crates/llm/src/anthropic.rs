use crate::{
    recording::{APIRecorder, PlaybackState},
    streaming::{ChunkStream, HttpChunkStream, PlaybackChunkStream},
    types::*,
    utils, ApiError, ApiErrorContext, LLMProvider, RateLimitHandler, StreamingCallback,
    StreamingChunk,
};
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};
use std::str::{self};
use std::time::{Duration, SystemTime};
use tracing::{debug, warn};

/// Trait for providing authentication headers
#[async_trait]
pub trait AuthProvider: Send + Sync {
    async fn get_auth_headers(&self) -> Result<Vec<(String, String)>>;
}

/// Trait for customizing requests before sending
pub trait RequestCustomizer: Send + Sync {
    fn customize_request(&self, request: &mut serde_json::Value) -> Result<()>;
    fn get_additional_headers(&self) -> Vec<(String, String)>;
    fn customize_url(&self, base_url: &str, streaming: bool) -> String;
}

/// Trait for converting messages to the appropriate format
pub trait MessageConverter: Send + Sync {
    fn convert_messages(&mut self, messages: Vec<Message>) -> Result<Vec<serde_json::Value>>;
}

/// Default API key authentication provider
pub struct ApiKeyAuth {
    api_key: String,
}

impl ApiKeyAuth {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

#[async_trait]
impl AuthProvider for ApiKeyAuth {
    async fn get_auth_headers(&self) -> Result<Vec<(String, String)>> {
        Ok(vec![("x-api-key".to_string(), self.api_key.clone())])
    }
}

/// Default request customizer for Anthropic API
pub struct DefaultRequestCustomizer;

impl RequestCustomizer for DefaultRequestCustomizer {
    fn customize_request(&self, _request: &mut serde_json::Value) -> Result<()> {
        Ok(())
    }

    fn get_additional_headers(&self) -> Vec<(String, String)> {
        vec![("anthropic-version".to_string(), "2023-06-01".to_string())]
    }

    fn customize_url(&self, base_url: &str, _streaming: bool) -> String {
        format!("{base_url}/messages")
    }
}

/// Default message converter with Anthropic caching logic
pub struct DefaultMessageConverter;

impl Default for DefaultMessageConverter {
    fn default() -> Self {
        Self
    }
}

impl DefaultMessageConverter {
    pub fn new() -> Self {
        Self
    }

    /// Get cache marker positions based purely on message count
    /// 0-4 messages: no cache markers
    /// 5-9 messages: marker at index 4
    /// 10-14 messages: markers at indices 4 and 9
    /// 15-19 messages: markers at indices 9 and 14
    /// 20-24 messages: markers at indices 14 and 19
    /// etc.
    fn get_cache_marker_positions(&self, messages: &[Message]) -> Vec<usize> {
        if messages.len() < 5 {
            return vec![];
        }
        let remainder = messages.len() % 5;
        let last_marker = messages.len() - remainder;
        if last_marker > 5 {
            vec![last_marker - 6, last_marker - 1]
        } else {
            vec![last_marker - 1]
        }
    }

    /// Convert generic messages to Anthropic-specific format with cache control
    fn convert_messages_with_cache(&self, messages: Vec<Message>) -> Vec<AnthropicMessage> {
        let cache_positions = self.get_cache_marker_positions(&messages);

        messages
            .into_iter()
            .enumerate()
            .map(|(msg_index, msg)| {
                let should_cache = cache_positions.contains(&msg_index);

                let content_blocks = match msg.content {
                    MessageContent::Text(text) => {
                        vec![AnthropicContentBlock {
                            block_type: "text".to_string(),
                            content: AnthropicBlockContent::Text { text },
                            cache_control: if should_cache {
                                Some(CacheControl {
                                    cache_type: "ephemeral".to_string(),
                                })
                            } else {
                                None
                            },
                        }]
                    }
                    MessageContent::Structured(blocks) => {
                        let mut cache_applied = false;
                        blocks
                            .into_iter()
                            .map(|block| {
                                let (block_type, content, cache_eligible) = match block {
                                    ContentBlock::Text { text, .. } => (
                                        "text".to_string(),
                                        AnthropicBlockContent::Text { text },
                                        true,
                                    ),
                                    ContentBlock::Image {
                                        media_type, data, ..
                                    } => (
                                        "image".to_string(),
                                        AnthropicBlockContent::Image {
                                            source: AnthropicImageSource {
                                                source_type: "base64".to_string(),
                                                media_type,
                                                data,
                                            },
                                        },
                                        true,
                                    ),
                                    ContentBlock::ToolUse {
                                        id, name, input, ..
                                    } => (
                                        "tool_use".to_string(),
                                        AnthropicBlockContent::ToolUse { id, name, input },
                                        true,
                                    ),
                                    ContentBlock::ToolResult {
                                        tool_use_id,
                                        content,
                                        is_error,
                                        ..
                                    } => (
                                        "tool_result".to_string(),
                                        AnthropicBlockContent::ToolResult {
                                            tool_use_id,
                                            content,
                                            is_error,
                                        },
                                        true,
                                    ),
                                    ContentBlock::Thinking {
                                        thinking,
                                        signature,
                                        ..
                                    } => (
                                        "thinking".to_string(),
                                        AnthropicBlockContent::Thinking {
                                            thinking,
                                            signature,
                                        },
                                        false,
                                    ),
                                    ContentBlock::RedactedThinking { data, .. } => (
                                        "redacted_thinking".to_string(),
                                        AnthropicBlockContent::RedactedThinking { data },
                                        false,
                                    ),
                                };

                                let cache_control =
                                    if should_cache && cache_eligible && !cache_applied {
                                        cache_applied = true;
                                        Some(CacheControl {
                                            cache_type: "ephemeral".to_string(),
                                        })
                                    } else {
                                        None
                                    };

                                AnthropicContentBlock {
                                    block_type,
                                    content,
                                    cache_control,
                                }
                            })
                            .collect()
                    }
                };

                AnthropicMessage {
                    role: match msg.role {
                        MessageRole::User => "user".to_string(),
                        MessageRole::Assistant => "assistant".to_string(),
                    },
                    content: content_blocks,
                }
            })
            .collect()
    }
}

impl MessageConverter for DefaultMessageConverter {
    fn convert_messages(&mut self, messages: Vec<Message>) -> Result<Vec<serde_json::Value>> {
        let anthropic_messages = self.convert_messages_with_cache(messages);
        Ok(vec![serde_json::to_value(anthropic_messages)?])
    }
}

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

/// Anthropic-specific content block with cache control support
#[derive(Debug, Serialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(flatten)]
    content: AnthropicBlockContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

/// Content variants for Anthropic content blocks
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum AnthropicBlockContent {
    Text {
        text: String,
    },
    Image {
        source: AnthropicImageSource,
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
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

/// Anthropic image source structure
#[derive(Debug, Serialize)]
struct AnthropicImageSource {
    #[serde(rename = "type")]
    source_type: String,
    media_type: String,
    data: String,
}

/// Anthropic-specific message structure
#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: Vec<AnthropicContentBlock>,
}

#[derive(Debug, Serialize)]
struct ThinkingConfiguration {
    #[serde(rename = "type")]
    thinking_type: String,
    budget_tokens: usize,
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

pub struct AnthropicClient {
    client: Client,
    base_url: String,
    model: String,
    recorder: Option<APIRecorder>,
    playback: Option<PlaybackState>,

    // Customization points
    auth_provider: Box<dyn AuthProvider>,
    request_customizer: Box<dyn RequestCustomizer>,
    message_converter: Box<dyn MessageConverter>,

    // Custom model configuration to merge into API requests
    custom_config: Option<serde_json::Value>,
}

impl AnthropicClient {
    /// Substrings of model IDs that should enable thinking mode and higher limits
    fn thinking_model_substrings() -> &'static [&'static str] {
        &["claude-sonnet-4", "claude-3-7-sonnet", "claude-opus-4"]
    }

    /// Returns true if the current model should have thinking mode enabled
    fn supports_thinking(&self) -> bool {
        Self::thinking_model_substrings()
            .iter()
            .any(|substr| self.model.contains(substr))
    }

    pub fn default_base_url() -> String {
        "https://api.anthropic.com/v1".to_string()
    }

    pub fn new(api_key: String, model: String, base_url: String) -> Self {
        Self {
            client: Client::new(),
            base_url,
            model,
            recorder: None,
            playback: None,
            auth_provider: Box::new(ApiKeyAuth::new(api_key)),
            request_customizer: Box::new(DefaultRequestCustomizer),
            message_converter: Box::new(DefaultMessageConverter::new()),
            custom_config: None,
        }
    }

    /// Create a new client with recording capability
    pub fn new_with_recorder<P: AsRef<std::path::Path>>(
        api_key: String,
        model: String,
        base_url: String,
        recording_path: P,
    ) -> Self {
        Self {
            client: Client::new(),
            base_url,
            model,
            recorder: Some(APIRecorder::new(recording_path)),
            playback: None,
            auth_provider: Box::new(ApiKeyAuth::new(api_key)),
            request_customizer: Box::new(DefaultRequestCustomizer),
            message_converter: Box::new(DefaultMessageConverter::new()),
            custom_config: None,
        }
    }

    /// New constructor for customization
    pub fn with_customization(
        model: String,
        base_url: String,
        auth_provider: Box<dyn AuthProvider>,
        request_customizer: Box<dyn RequestCustomizer>,
        message_converter: Box<dyn MessageConverter>,
    ) -> Self {
        Self {
            client: Client::new(),
            base_url,
            model,
            recorder: None,
            playback: None,
            auth_provider,
            request_customizer,
            message_converter,
            custom_config: None,
        }
    }

    /// Set recorder for existing client
    pub fn with_recorder<P: AsRef<std::path::Path>>(mut self, recording_path: P) -> Self {
        self.recorder = Some(APIRecorder::new(recording_path));
        self
    }

    /// Add playback capability to the client
    pub fn with_playback(mut self, playback_state: PlaybackState) -> Self {
        self.playback = Some(playback_state);
        self
    }

    /// Set custom model configuration to be merged into API requests
    pub fn with_custom_config(mut self, custom_config: serde_json::Value) -> Self {
        self.custom_config = Some(custom_config);
        self
    }

    fn get_url(&self, streaming: bool) -> String {
        self.request_customizer
            .customize_url(&self.base_url, streaming)
    }

    async fn send_with_retry(
        &mut self,
        request: &serde_json::Value,
        streaming_callback: Option<&StreamingCallback>,
        max_retries: u32,
    ) -> Result<LLMResponse> {
        // If playback is enabled, skip HTTP and use recorded data
        if self.playback.is_some() {
            return self.playback_request(request, streaming_callback).await;
        }

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

    async fn playback_request(
        &mut self,
        _request: &serde_json::Value,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        let playback = self.playback.as_ref().unwrap();

        // Get the next session from the recording
        let session = playback
            .next_session()
            .ok_or_else(|| anyhow::anyhow!("No more recorded sessions available"))?;

        debug!("Playing back session with {} chunks", session.chunks.len());

        if let Some(callback) = streaming_callback {
            // Use the common PlaybackChunkStream and existing streaming processing logic
            let mut chunk_stream = PlaybackChunkStream::new(session.chunks.clone(), playback.fast);
            let rate_limits = AnthropicRateLimitInfo::default();

            // Use the same streaming processing logic, but without recording
            self.process_chunk_stream_without_recording(&mut chunk_stream, callback, rate_limits)
                .await
                .map(|(response, _)| response)
        } else {
            // Non-streaming playback - parse the recorded response body from chunks
            let body: String = session.chunks.iter().map(|c| c.data.as_str()).collect();

            let anthropic_response: AnthropicResponse = serde_json::from_str(&body)
                .map_err(|e| anyhow::anyhow!("Failed to parse recorded response body: {e}"))?;

            let content = anthropic_response.content;
            let usage = Usage {
                input_tokens: anthropic_response.usage.input_tokens,
                output_tokens: anthropic_response.usage.output_tokens,
                cache_creation_input_tokens: anthropic_response.usage.cache_creation_input_tokens,
                cache_read_input_tokens: anthropic_response.usage.cache_read_input_tokens,
            };

            Ok(LLMResponse {
                content,
                usage,
                rate_limit_info: None,
            })
        }
    }

    async fn try_send_request(
        &mut self,
        request: &serde_json::Value,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<(LLMResponse, AnthropicRateLimitInfo)> {
        let accept_value = if streaming_callback.is_some() {
            "text/event-stream"
        } else {
            "application/json"
        };

        // Start recording before HTTP request to capture real latency
        if let Some(recorder) = &self.recorder {
            recorder.start_recording(request.clone())?;
        }

        // Get auth headers
        let auth_headers = self.auth_provider.get_auth_headers().await?;

        // Build request
        let mut request_builder = self
            .client
            .post(self.get_url(streaming_callback.is_some()))
            .header("accept", accept_value);

        // Add auth headers
        for (key, value) in auth_headers {
            request_builder = request_builder.header(key, value);
        }

        // Add additional headers
        for (key, value) in self.request_customizer.get_additional_headers() {
            request_builder = request_builder.header(key, value);
        }

        // Add model-specific headers for thinking-enabled models
        if self.supports_thinking() {
            request_builder = request_builder.header(
                "anthropic-beta",
                "output-128k-2025-02-19,interleaved-thinking-2025-05-14",
            );
        }

        let response = request_builder
            .json(request)
            .send()
            .await
            .map_err(|e| ApiError::NetworkError(e.to_string()))?;

        // Log raw headers for debugging
        debug!("Response headers: {:?}", response.headers());

        let response = utils::check_response_error::<AnthropicRateLimitInfo>(response).await?;
        let rate_limits = AnthropicRateLimitInfo::from_response(&response);

        // Log parsed rate limits
        debug!("Parsed rate limits: {:?}", rate_limits);

        if let Some(callback) = streaming_callback {
            let mut chunk_stream = HttpChunkStream::new(response);
            self.process_chunk_stream(&mut chunk_stream, callback, rate_limits)
                .await
        } else {
            let response_text = response
                .text()
                .await
                .map_err(|e| ApiError::NetworkError(e.to_string()))?;

            // Record full non-streaming response body
            if let Some(recorder) = &self.recorder {
                if let Err(e) = recorder.record_chunk(&response_text) {
                    warn!("Failed to record non-streaming response: {e}");
                }
                if let Err(e) = recorder.end_recording() {
                    warn!("Failed to end recording: {e}");
                }
            }

            let anthropic_response: AnthropicResponse = serde_json::from_str(&response_text)
                .map_err(|e| ApiError::Unknown(format!("Failed to parse response: {e}")))?;

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
                rate_limit_info: Some(crate::types::RateLimitInfo {
                    tokens_limit: rate_limits.tokens_limit,
                    tokens_remaining: rate_limits.tokens_remaining,
                }),
            };

            Ok((llm_response, rate_limits))
        }
    }

    async fn process_chunk_stream(
        &self,
        chunk_stream: &mut dyn ChunkStream,
        callback: &StreamingCallback,
        rate_limits: AnthropicRateLimitInfo,
    ) -> Result<(LLMResponse, AnthropicRateLimitInfo)> {
        self.process_chunk_stream_with_recorder(chunk_stream, callback, rate_limits, &self.recorder)
            .await
    }

    async fn process_chunk_stream_without_recording(
        &self,
        chunk_stream: &mut dyn ChunkStream,
        callback: &StreamingCallback,
        rate_limits: AnthropicRateLimitInfo,
    ) -> Result<(LLMResponse, AnthropicRateLimitInfo)> {
        self.process_chunk_stream_with_recorder(chunk_stream, callback, rate_limits, &None)
            .await
    }

    async fn process_chunk_stream_with_recorder(
        &self,
        chunk_stream: &mut dyn ChunkStream,
        callback: &StreamingCallback,
        rate_limits: AnthropicRateLimitInfo,
        recorder: &Option<APIRecorder>,
    ) -> Result<(LLMResponse, AnthropicRateLimitInfo)> {
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
                debug!("Received stream event: {}", data);
                // Record the raw SSE line if recorder is available
                if let Some(recorder) = &recorder {
                    recorder.record_chunk(line)?;
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
                            _ => Err(anyhow::anyhow!("Stream error: {error_msg}")),
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
                            usage.cache_read_input_tokens = message.usage.cache_read_input_tokens;
                            return Ok(());
                        }
                        StreamEvent::MessageDelta { usage: delta_usage } => {
                            // Use max() to ensure counts never decrease during streaming
                            usage.input_tokens = usage.input_tokens.max(delta_usage.input_tokens);
                            usage.output_tokens = delta_usage.output_tokens;
                            usage.cache_creation_input_tokens = usage
                                .cache_creation_input_tokens
                                .max(delta_usage.cache_creation_input_tokens);
                            usage.cache_read_input_tokens = usage
                                .cache_read_input_tokens
                                .max(delta_usage.cache_read_input_tokens);
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
                                        start_time: Some(SystemTime::now()),
                                        end_time: None,
                                    }
                                }
                                "redacted_thinking" => {
                                    if let Some(data) = content_block.data {
                                        current_content.push_str(&data);
                                    }
                                    ContentBlock::RedactedThinking {
                                        id: String::new(),
                                        summary: vec![], // Empty for Anthropic
                                        data: current_content.clone(),
                                        start_time: Some(SystemTime::now()),
                                        end_time: None,
                                    }
                                }
                                "text" => {
                                    if let Some(text) = content_block.text {
                                        current_content.push_str(&text);
                                    }
                                    ContentBlock::Text {
                                        text: current_content.clone(),
                                        start_time: Some(SystemTime::now()),
                                        end_time: None,
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
                                        start_time: Some(SystemTime::now()),
                                        end_time: None,
                                    }
                                }
                                _ => ContentBlock::Text {
                                    text: String::new(),
                                    start_time: Some(SystemTime::now()),
                                    end_time: None,
                                },
                            };
                            blocks.push(block);
                        }
                        StreamEvent::ContentBlockDelta { delta, .. } => {
                            match &delta {
                                ContentDelta::Thinking {
                                    thinking: delta_text,
                                } => {
                                    current_content.push_str(delta_text);
                                    callback(&StreamingChunk::Thinking(delta_text.clone()))?;
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
                                    current_content.push_str(delta_text);
                                    callback(&StreamingChunk::Text(delta_text.clone()))?;
                                }
                                ContentDelta::InputJson { partial_json } => {
                                    let (tool_name, tool_id) =
                                        blocks.last().map_or((None, None), |block| {
                                            if let ContentBlock::ToolUse { name, id, .. } = block {
                                                (Some(name.clone()), Some(id.clone()))
                                            } else {
                                                warn!("Last block is not a ToolUse type!");
                                                (None, None)
                                            }
                                        });

                                    current_content.push_str(partial_json);
                                    callback(&StreamingChunk::InputJson {
                                        content: partial_json.clone(),
                                        tool_name,
                                        tool_id,
                                    })?;
                                }
                            }
                        }
                        StreamEvent::ContentBlockStop { .. } => {
                            let now = SystemTime::now();
                            match blocks.last_mut().unwrap() {
                                ContentBlock::Thinking {
                                    thinking, end_time, ..
                                } => {
                                    *thinking = current_content.clone();
                                    *end_time = Some(now);
                                }
                                ContentBlock::Text { text, end_time, .. } => {
                                    *text = current_content.clone();
                                    *end_time = Some(now);
                                }
                                ContentBlock::ToolUse {
                                    input, end_time, ..
                                } => {
                                    if let Ok(json) = serde_json::from_str(current_content) {
                                        *input = json;
                                    }
                                    *end_time = Some(now);
                                }
                                ContentBlock::RedactedThinking { end_time, .. } => {
                                    *end_time = Some(now);
                                }
                                ContentBlock::Image { end_time, .. } => {
                                    *end_time = Some(now);
                                }
                                ContentBlock::ToolResult { end_time, .. } => {
                                    *end_time = Some(now);
                                }
                            }
                        }
                        _ => {}
                    }
                } else {
                    return Err(anyhow::anyhow!("Failed to parse stream event:\n{line}"));
                }
            }
            Ok(())
        }

        while let Some(chunk) = chunk_stream.next_chunk().await? {
            match process_chunk(
                &chunk,
                &mut line_buffer,
                &mut blocks,
                &mut usage,
                &mut current_content,
                callback,
                recorder,
            ) {
                Ok(()) => continue,
                Err(e) if e.to_string().contains("Tool limit reached") => {
                    debug!(
                        "Tool limit reached, stopping streaming early. Collected {} blocks so far",
                        blocks.len()
                    );

                    // Finalize the current block with any accumulated content
                    if !blocks.is_empty() && !current_content.is_empty() {
                        let now = SystemTime::now();
                        match blocks.last_mut().unwrap() {
                            ContentBlock::Thinking {
                                thinking, end_time, ..
                            } => {
                                *thinking = current_content.clone();
                                *end_time = Some(now);
                            }
                            ContentBlock::Text { text, end_time, .. } => {
                                *text = current_content.clone();
                                *end_time = Some(now);
                            }
                            ContentBlock::ToolUse {
                                input, end_time, ..
                            } => {
                                if let Ok(json) = serde_json::from_str(&current_content) {
                                    *input = json;
                                }
                                *end_time = Some(now);
                            }
                            _ => {}
                        }
                    }

                    line_buffer.clear(); // Make sure we stop processing
                    break; // Exit chunk processing loop early
                }
                Err(e) => return Err(e), // Propagate other errors
            }
        }

        // Process any remaining data in the buffer
        if !line_buffer.is_empty() {
            process_sse_line(
                &line_buffer,
                &mut blocks,
                &mut usage,
                &mut current_content,
                callback,
                recorder,
            )?;
        }

        // Ensure any incomplete blocks have end times set
        let now = SystemTime::now();
        for block in blocks.iter_mut() {
            match block {
                ContentBlock::Thinking { end_time, .. }
                | ContentBlock::RedactedThinking { end_time, .. }
                | ContentBlock::Text { end_time, .. }
                | ContentBlock::Image { end_time, .. }
                | ContentBlock::ToolUse { end_time, .. }
                | ContentBlock::ToolResult { end_time, .. } => {
                    if end_time.is_none() {
                        *end_time = Some(now);
                    }
                }
            }
        }

        // Send StreamingComplete to indicate streaming has finished
        callback(&StreamingChunk::StreamingComplete)?;

        // End recording if a recorder is available
        if let Some(recorder) = recorder {
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
                rate_limit_info: Some(crate::types::RateLimitInfo {
                    tokens_limit: rate_limits.tokens_limit,
                    tokens_remaining: rate_limits.tokens_remaining,
                }),
            },
            rate_limits,
        ))
    }
}

#[async_trait]
impl LLMProvider for AnthropicClient {
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
                "type": "auto",
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

        // Configure thinking mode and max_tokens based on model
        let (thinking_config, max_tokens) = if self.supports_thinking() {
            (
                Some(ThinkingConfiguration {
                    thinking_type: "enabled".to_string(),
                    budget_tokens: 16000,
                }),
                64000,
            )
        } else {
            (None, 8192)
        };

        // Convert messages using the message converter
        let converted_messages = self.message_converter.convert_messages(request.messages)?;
        let messages_json = converted_messages.into_iter().next().unwrap_or_default();

        let mut anthropic_request = serde_json::json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "temperature": if thinking_config.is_some() {
                // Anthropic requires this to be 1.0 if you enable "thinking"
                1.0
            } else {
                0.7
            },
            "system": system,
            "stream": streaming_callback.is_some(),
            "messages": messages_json,
        });

        if let Some(thinking_config) = thinking_config {
            anthropic_request["thinking"] = serde_json::to_value(thinking_config)?;
        }
        if let Some(tool_choice) = tool_choice {
            anthropic_request["tool_choice"] = tool_choice;
        }

        if let Some(tools) = tools {
            anthropic_request["tools"] = serde_json::to_value(tools)?;
        }

        // Apply custom model configuration if present
        if let Some(ref custom_config) = self.custom_config {
            anthropic_request =
                crate::config_merge::merge_json(anthropic_request, custom_config.clone());
        }

        // Allow request customizer to modify the request
        self.request_customizer
            .customize_request(&mut anthropic_request)?;

        let request_start = std::time::SystemTime::now();
        let mut response = self
            .send_with_retry(&anthropic_request, streaming_callback, 3)
            .await?;
        let response_end = std::time::SystemTime::now();

        // For non-streaming responses, distribute timestamps across blocks
        if streaming_callback.is_none() {
            response.set_distributed_timestamps(request_start, response_end);
        }

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    /// Test cache marker placement based on message count (stateless)
    #[test]
    fn test_message_count_based_cache_markers() {
        let converter = DefaultMessageConverter::new();

        // Helper function to create a simple text message
        fn create_message(role: MessageRole, text: &str) -> Message {
            Message {
                role,
                content: MessageContent::Text(text.to_string()),
                request_id: None,
                usage: None,
            }
        }

        // Helper to count cache markers in messages
        fn count_message_cache_markers(result: &[AnthropicMessage]) -> Vec<usize> {
            let mut marker_positions = Vec::new();
            for (msg_idx, msg) in result.iter().enumerate() {
                if msg
                    .content
                    .iter()
                    .any(|block| block.cache_control.is_some())
                {
                    marker_positions.push(msg_idx);
                }
            }
            marker_positions
        }

        // Test 0-4 messages: No cache markers
        for msg_count in 0..=4 {
            let messages: Vec<Message> = (0..msg_count)
                .map(|i| create_message(MessageRole::User, &format!("Message {i}")))
                .collect();

            let result = converter.convert_messages_with_cache(messages);
            let markers = count_message_cache_markers(&result);
            assert!(
                markers.is_empty(),
                "{msg_count} messages: Should have no cache markers"
            );
        }

        // Test 5-9 messages: Cache marker at index 4
        for msg_count in 5..=9 {
            let messages: Vec<Message> = (0..msg_count)
                .map(|i| create_message(MessageRole::User, &format!("Message {i}")))
                .collect();

            let result = converter.convert_messages_with_cache(messages);
            let markers = count_message_cache_markers(&result);
            assert_eq!(
                markers,
                vec![4],
                "{msg_count} messages: Should have cache marker at index 4"
            );
        }

        // Test 10-14 messages: Cache markers at indices 4 and 9
        for msg_count in 10..=14 {
            let messages: Vec<Message> = (0..msg_count)
                .map(|i| create_message(MessageRole::User, &format!("Message {i}")))
                .collect();

            let result = converter.convert_messages_with_cache(messages);
            let markers = count_message_cache_markers(&result);
            assert_eq!(
                markers,
                vec![4, 9],
                "{msg_count} messages: Should have cache markers at indices 4 and 9"
            );
        }

        // Test 15-19 messages: Cache markers at indices 9 and 14
        for msg_count in 15..=19 {
            let messages: Vec<Message> = (0..msg_count)
                .map(|i| create_message(MessageRole::User, &format!("Message {i}")))
                .collect();

            let result = converter.convert_messages_with_cache(messages);
            let markers = count_message_cache_markers(&result);
            assert_eq!(
                markers,
                vec![9, 14],
                "{msg_count} messages: Should have cache markers at indices 9 and 14"
            );
        }

        // Test 20-24 messages: Cache markers at indices 14 and 19
        for msg_count in 20..=24 {
            let messages: Vec<Message> = (0..msg_count)
                .map(|i| create_message(MessageRole::User, &format!("Message {i}")))
                .collect();

            let result = converter.convert_messages_with_cache(messages);
            let markers = count_message_cache_markers(&result);
            assert_eq!(
                markers,
                vec![14, 19],
                "{msg_count} messages: Should have cache markers at indices 14 and 19"
            );
        }
    }

    /// Test cache markers with tool interactions
    #[test]
    fn test_cache_markers_with_tools() {
        let converter = DefaultMessageConverter::new();

        // Helper to create tool use message
        fn create_tool_message(id: &str) -> Message {
            Message {
                role: MessageRole::Assistant,
                content: MessageContent::Structured(vec![
                    ContentBlock::Text {
                        text: "Using tool".to_string(),
                        start_time: None,
                        end_time: None,
                    },
                    ContentBlock::ToolUse {
                        id: id.to_string(),
                        name: "test_tool".to_string(),
                        input: json!({"param": "value"}),
                        start_time: None,
                        end_time: None,
                    },
                ]),
                request_id: None,
                usage: None,
            }
        }

        // Helper to create tool result message
        fn create_tool_result(tool_id: &str, content: &str) -> Message {
            Message {
                role: MessageRole::User,
                content: MessageContent::Structured(vec![ContentBlock::ToolResult {
                    tool_use_id: tool_id.to_string(),
                    content: content.to_string(),
                    is_error: Some(false),
                    start_time: None,
                    end_time: None,
                }]),
                request_id: None,
                usage: None,
            }
        }

        // Test with 15 messages (should have cache markers at indices 9 and 14)
        let mut messages = Vec::new();
        for i in 0..5 {
            messages.push(Message {
                role: MessageRole::User,
                content: MessageContent::Text(format!("Request {i}")),
                request_id: None,
                usage: None,
            });
            messages.push(create_tool_message(&format!("tool_{i}")));
            messages.push(create_tool_result(
                &format!("tool_{i}"),
                &format!("Result {i}"),
            ));
        }
        // Total: 15 messages

        let result = converter.convert_messages_with_cache(messages);

        // Should have cache markers at indices 9 and 14
        let mut cache_markers = Vec::new();
        for (idx, msg) in result.iter().enumerate() {
            if msg
                .content
                .iter()
                .any(|block| block.cache_control.is_some())
            {
                cache_markers.push(idx);
            }
        }
        assert_eq!(
            cache_markers,
            vec![9, 14],
            "15 messages should have cache markers at indices 9 and 14"
        );

        // Verify structured content is preserved
        assert_eq!(
            result[1].content.len(),
            2,
            "Tool message should have 2 blocks"
        );
        assert_eq!(result[1].content[0].block_type, "text");
        assert_eq!(result[1].content[1].block_type, "tool_use");

        assert_eq!(
            result[2].content.len(),
            1,
            "Tool result should have 1 block"
        );
        assert_eq!(result[2].content[0].block_type, "tool_result");
    }

    #[test]
    fn test_thinking_blocks_not_marked_for_cache_control() {
        let converter = DefaultMessageConverter::new();

        let mut messages: Vec<Message> = (0..4)
            .map(|i| Message {
                role: MessageRole::User,
                content: MessageContent::Text(format!("Prelude {i}")),
                request_id: None,
                usage: None,
            })
            .collect();

        messages.push(Message {
            role: MessageRole::Assistant,
            content: MessageContent::Structured(vec![
                ContentBlock::Thinking {
                    thinking: "internal reasoning".to_string(),
                    signature: "sig".to_string(),
                    start_time: None,
                    end_time: None,
                },
                ContentBlock::Text {
                    text: "Here is the result.".to_string(),
                    start_time: None,
                    end_time: None,
                },
            ]),
            request_id: None,
            usage: None,
        });

        let result = converter.convert_messages_with_cache(messages);

        let cached_message = &result[4];
        assert_eq!(cached_message.content.len(), 2);
        assert_eq!(cached_message.content[0].block_type, "thinking");
        assert!(cached_message.content[0].cache_control.is_none());
        assert_eq!(cached_message.content[1].block_type, "text");
        assert!(cached_message.content[1].cache_control.is_some());
        assert_eq!(
            cached_message
                .content
                .iter()
                .filter(|block| block.cache_control.is_some())
                .count(),
            1,
            "Only one content block should carry cache_control",
        );
    }

    /// Test that cache markers work across different message histories (agent-agnostic)
    #[test]
    fn test_agent_agnostic_cache_markers() {
        let converter = DefaultMessageConverter::new();

        // Create two different message histories with the same count (15)
        let messages_a: Vec<Message> = (0..15)
            .map(|i| Message {
                role: if i % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                content: MessageContent::Text(format!("Message A{i}")),
                request_id: None,
                usage: None,
            })
            .collect();

        let messages_b: Vec<Message> = (0..15)
            .map(|i| Message {
                role: if i % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                content: MessageContent::Text(format!("Completely different B{i}")),
                request_id: None,
                usage: None,
            })
            .collect();

        // Both should have identical cache marker placement (indices 9, 14)
        let result_a = converter.convert_messages_with_cache(messages_a);
        let result_b = converter.convert_messages_with_cache(messages_b);

        // Helper to extract marker positions
        let get_markers = |result: &[AnthropicMessage]| -> Vec<usize> {
            result
                .iter()
                .enumerate()
                .filter_map(|(idx, msg)| {
                    if msg
                        .content
                        .iter()
                        .any(|block| block.cache_control.is_some())
                    {
                        Some(idx)
                    } else {
                        None
                    }
                })
                .collect()
        };

        let markers_a = get_markers(&result_a);
        let markers_b = get_markers(&result_b);

        assert_eq!(
            markers_a,
            vec![9, 14],
            "Message set A should have markers at indices 9, 14"
        );
        assert_eq!(
            markers_b,
            vec![9, 14],
            "Message set B should have markers at indices 9, 14"
        );
        assert_eq!(
            markers_a, markers_b,
            "Both message sets should have identical marker placement"
        );

        // Test with different message counts to ensure it's truly stateless
        let messages_short: Vec<Message> = (0..7)
            .map(|i| Message {
                role: MessageRole::User,
                content: MessageContent::Text(format!("Short {i}")),
                request_id: None,
                usage: None,
            })
            .collect();

        let result_short = converter.convert_messages_with_cache(messages_short);
        let markers_short = get_markers(&result_short);
        assert_eq!(
            markers_short,
            vec![4],
            "7 messages should have marker at index 4"
        );
    }

    /// Sophisticated test that validates cache marker behavior with different message counts
    /// and tool interactions, demonstrating agent-agnostic stateless behavior
    #[test]
    fn test_sophisticated_cache_marker_injection() {
        let converter = DefaultMessageConverter::new();

        // Create a realistic conversation with mixed content types
        let mut messages = Vec::new();

        // Initial user request
        messages.push(Message {
            role: MessageRole::User,
            content: MessageContent::Text("Help me analyze this data".to_string()),
            request_id: None,
            usage: None,
        });

        // Assistant response with tool use
        messages.push(Message {
            role: MessageRole::Assistant,
            content: MessageContent::Structured(vec![
                ContentBlock::Text {
                    text: "I'll help you analyze the data using the appropriate tools.".to_string(),
                    start_time: None,
                    end_time: None,
                },
                ContentBlock::ToolUse {
                    id: "analysis_tool_1".to_string(),
                    name: "data_analyzer".to_string(),
                    input: json!({"dataset": "user_data.csv", "analysis_type": "statistical"}),
                    start_time: None,
                    end_time: None,
                },
            ]),
            request_id: None,
            usage: None,
        });

        // Tool result
        messages.push(Message {
            role: MessageRole::User,
            content: MessageContent::Structured(vec![ContentBlock::ToolResult {
                tool_use_id: "analysis_tool_1".to_string(),
                content: "Analysis complete: Mean=45.2, StdDev=12.8, Outliers=3".to_string(),
                is_error: Some(false),
                start_time: None,
                end_time: None,
            }]),
            request_id: None,
            usage: None,
        });

        // Continue building conversation to 20+ messages
        for i in 1..=9 {
            messages.push(Message {
                role: MessageRole::User,
                content: MessageContent::Text(format!("Follow up question {i}")),
                request_id: None,
                usage: None,
            });

            messages.push(Message {
                role: MessageRole::Assistant,
                content: MessageContent::Structured(vec![
                    ContentBlock::Text {
                        text: format!("Let me help with question {i}."),
                        start_time: None,
                        end_time: None,
                    },
                    ContentBlock::ToolUse {
                        id: format!("tool_{i}"),
                        name: "helper_tool".to_string(),
                        input: json!({"query": format!("query_{}", i), "context": "analysis"}),
                        start_time: None,
                        end_time: None,
                    },
                ]),
                request_id: None,
                usage: None,
            });

            messages.push(Message {
                role: MessageRole::User,
                content: MessageContent::Structured(vec![ContentBlock::ToolResult {
                    tool_use_id: format!("tool_{i}"),
                    content: format!("Tool result for query {i}"),
                    is_error: Some(false),
                    start_time: None,
                    end_time: None,
                }]),
                request_id: None,
                usage: None,
            });
        }

        // We now have 30 messages total (3 initial + 27 from loop)
        assert_eq!(
            messages.len(),
            30,
            "Should have 30 messages for comprehensive testing"
        );

        // Helper to extract cache marker positions
        let get_markers = |result: &[AnthropicMessage]| -> Vec<usize> {
            result
                .iter()
                .enumerate()
                .filter_map(|(idx, msg)| {
                    if msg
                        .content
                        .iter()
                        .any(|block| block.cache_control.is_some())
                    {
                        Some(idx)
                    } else {
                        None
                    }
                })
                .collect()
        };

        // Test with the full 30-message conversation
        let result30 = converter.convert_messages_with_cache(messages.clone());
        let markers30 = get_markers(&result30);
        assert_eq!(
            markers30,
            vec![24, 29],
            "30 messages: Should have cache markers at indices 24 and 29, found: {markers30:?}"
        );

        // Test different message counts to demonstrate stateless behavior
        // Simulate different agents calling with different conversation lengths

        // Agent 1: Short conversation (7 messages)
        let short_messages = messages[..7].to_vec();
        let result_short = converter.convert_messages_with_cache(short_messages);
        let markers_short = get_markers(&result_short);
        assert_eq!(
            markers_short,
            vec![4],
            "7 messages should have marker at index 4"
        );

        // Agent 2: Medium conversation (12 messages)
        let medium_messages = messages[..12].to_vec();
        let result_medium = converter.convert_messages_with_cache(medium_messages);
        let markers_medium = get_markers(&result_medium);
        assert_eq!(
            markers_medium,
            vec![4, 9],
            "12 messages should have markers at indices 4, 9"
        );

        // Agent 3: Long conversation (18 messages)
        let long_messages = messages[..18].to_vec();
        let result_long = converter.convert_messages_with_cache(long_messages);
        let markers_long = get_markers(&result_long);
        assert_eq!(
            markers_long,
            vec![9, 14],
            "18 messages should have markers at indices 9, 14"
        );

        // Agent 4: Very long conversation (25 messages)
        let very_long_messages = messages[..25].to_vec();
        let result_very_long = converter.convert_messages_with_cache(very_long_messages);
        let markers_very_long = get_markers(&result_very_long);
        assert_eq!(
            markers_very_long,
            vec![19, 24],
            "25 messages should have markers at indices 19, 24"
        );

        // Verify that cache markers are only placed on first content block of structured messages
        let structured_message = &result30[1]; // This should be an assistant message with tool use
        assert_eq!(
            structured_message.content.len(),
            2,
            "Assistant message should have 2 content blocks"
        );
        assert_eq!(structured_message.content[0].block_type, "text");
        assert_eq!(structured_message.content[1].block_type, "tool_use");

        // If this message has a cache marker, it should only be on the first block
        if let Some(marker_index) = structured_message
            .content
            .iter()
            .position(|block| block.cache_control.is_some())
        {
            assert_eq!(
                marker_index, 0,
                "Cache marker should only be on first eligible block"
            );
            assert!(
                structured_message
                    .content
                    .iter()
                    .enumerate()
                    .all(|(idx, block)| idx == marker_index || block.cache_control.is_none()),
                "Only a single content block should carry cache_control"
            );
        }

        // Verify tool result messages are properly formatted
        let tool_result_message = &result30[2]; // This should be a tool result message
        assert_eq!(
            tool_result_message.content.len(),
            1,
            "Tool result message should have 1 content block"
        );
        assert_eq!(tool_result_message.content[0].block_type, "tool_result");

        if let AnthropicBlockContent::ToolResult {
            tool_use_id,
            content,
            is_error,
        } = &tool_result_message.content[0].content
        {
            assert_eq!(tool_use_id, "analysis_tool_1");
            assert!(content.contains("Analysis complete"));
            assert_eq!(*is_error, Some(false));
        } else {
            panic!("Expected ToolResult content");
        }

        // Test with fake tool results (error case)
        let mut error_messages = messages.clone();
        error_messages.push(Message {
            role: MessageRole::User,
            content: MessageContent::Text("Try something that might fail".to_string()),
            request_id: None,
            usage: None,
        });

        error_messages.push(Message {
            role: MessageRole::Assistant,
            content: MessageContent::Structured(vec![
                ContentBlock::Text {
                    text: "I'll try that operation.".to_string(),
                    start_time: None,
                    end_time: None,
                },
                ContentBlock::ToolUse {
                    id: "risky_tool".to_string(),
                    name: "risky_operation".to_string(),
                    input: json!({"operation": "dangerous_task", "safety": false}),
                    start_time: None,
                    end_time: None,
                },
            ]),
            request_id: None,
            usage: None,
        });

        error_messages.push(Message {
            role: MessageRole::User,
            content: MessageContent::Structured(vec![ContentBlock::ToolResult {
                tool_use_id: "risky_tool".to_string(),
                content: "ERROR: Operation failed due to safety constraints".to_string(),
                is_error: Some(true),
                start_time: None,
                end_time: None,
            }]),
            request_id: None,
            usage: None,
        });

        // Convert with error scenario
        let error_result = converter.convert_messages_with_cache(error_messages);

        // Find and verify the error tool result
        let error_tool_result = error_result
            .iter()
            .find(|msg| {
                msg.content.iter().any(|block| {
                    matches!(
                        block.content,
                        AnthropicBlockContent::ToolResult {
                            is_error: Some(true),
                            ..
                        }
                    )
                })
            })
            .expect("Should find error tool result message");

        let error_block = error_tool_result
            .content
            .iter()
            .find(|block| block.block_type == "tool_result")
            .expect("Should find tool_result block");

        if let AnthropicBlockContent::ToolResult {
            tool_use_id,
            content,
            is_error,
        } = &error_block.content
        {
            assert_eq!(tool_use_id, "risky_tool");
            assert!(content.contains("ERROR"));
            assert_eq!(*is_error, Some(true));
        } else {
            panic!("Expected ToolResult content");
        }
    }
}
