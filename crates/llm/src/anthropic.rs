use crate::{
    recording::APIRecorder, types::*, utils, ApiError, ApiErrorContext, LLMProvider,
    RateLimitHandler, StreamingCallback, StreamingChunk,
};
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};
use std::collections::{hash_map::DefaultHasher, HashMap};
use std::hash::{Hash, Hasher};
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
        format!("{}/messages", base_url)
    }
}

/// Default message converter with Anthropic caching logic
pub struct DefaultMessageConverter {
    cache_state: HashMap<String, CacheEntry>,
}

impl Default for DefaultMessageConverter {
    fn default() -> Self {
        Self {
            cache_state: HashMap::new(),
        }
    }
}

impl DefaultMessageConverter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Generate a stable cache key based on message windows
    fn generate_stable_cache_key(&self, messages: &[Message]) -> String {
        // Stable window: new hash every 10 messages
        let stable_count = (messages.len() / 10) * 10;

        // Hash the first stable_count messages
        let stable_messages = &messages[..stable_count.min(messages.len())];

        let mut hasher = DefaultHasher::new();
        for msg in stable_messages {
            // Hash role + content
            match msg.role {
                MessageRole::User => "user".hash(&mut hasher),
                MessageRole::Assistant => "assistant".hash(&mut hasher),
            };
            match &msg.content {
                MessageContent::Text(text) => {
                    text.hash(&mut hasher);
                }
                MessageContent::Structured(blocks) => {
                    for block in blocks {
                        // Hash block discriminant and content
                        std::mem::discriminant(block).hash(&mut hasher);
                        match block {
                            ContentBlock::Text { text } => text.hash(&mut hasher),
                            ContentBlock::ToolUse { id, name, input } => {
                                id.hash(&mut hasher);
                                name.hash(&mut hasher);
                                input.to_string().hash(&mut hasher);
                            }
                            ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                            } => {
                                tool_use_id.hash(&mut hasher);
                                content.hash(&mut hasher);
                                is_error.hash(&mut hasher);
                            }
                            ContentBlock::Thinking {
                                thinking,
                                signature,
                            } => {
                                thinking.hash(&mut hasher);
                                signature.hash(&mut hasher);
                            }
                            ContentBlock::RedactedThinking { data } => {
                                data.hash(&mut hasher);
                            }
                        }
                    }
                }
            }
        }

        format!("cache_{}_{:x}", stable_count, hasher.finish())
    }

    /// Determine cache marker positions (old and new for smooth transitions)
    fn get_cache_marker_positions(
        &mut self,
        messages: &[Message],
    ) -> (Option<usize>, Option<usize>) {
        let cache_key = self.generate_stable_cache_key(messages);

        // Cleanup old entries (>5min = Anthropic cache lifetime)
        let now = SystemTime::now();
        self.cache_state.retain(|_, entry| {
            now.duration_since(entry.last_used)
                .unwrap_or(Duration::from_secs(0))
                < Duration::from_secs(300)
        });

        let cache_key_for_debug = cache_key.clone();
        let entry = self.cache_state.entry(cache_key).or_insert(CacheEntry {
            count: 0,
            last_used: now,
            current_cache_position: None,
        });

        entry.count += 1;
        entry.last_used = now;

        // Move cache-marker every 5 requests
        if entry.count % 5 == 0 {
            // Calc new cache-marker position
            let stable_count = (messages.len() / 10) * 10;
            let new_cache_position = stable_count.saturating_sub(1);

            let old_position = entry.current_cache_position;
            entry.current_cache_position = Some(new_cache_position);

            debug!(
                "Moving cache marker from {:?} to {} for cache key {} (count: {})",
                old_position, new_cache_position, cache_key_for_debug, entry.count
            );

            // Return old and new marker
            (old_position, Some(new_cache_position))
        } else {
            // Return only the current marker
            (entry.current_cache_position, None)
        }
    }

    /// Convert generic messages to Anthropic-specific format with cache control
    fn convert_messages_with_cache(&mut self, messages: Vec<Message>) -> Vec<AnthropicMessage> {
        let (old_cache_position, new_cache_position) = self.get_cache_marker_positions(&messages);

        messages
            .into_iter()
            .enumerate()
            .map(|(msg_index, msg)| {
                let content_blocks = match msg.content {
                    MessageContent::Text(text) => {
                        vec![AnthropicContentBlock {
                            block_type: "text".to_string(),
                            content: AnthropicBlockContent::Text { text },
                            cache_control: if old_cache_position == Some(msg_index)
                                || new_cache_position == Some(msg_index)
                            {
                                Some(CacheControl {
                                    cache_type: "ephemeral".to_string(),
                                })
                            } else {
                                None
                            },
                        }]
                    }
                    MessageContent::Structured(blocks) => blocks
                        .into_iter()
                        .enumerate()
                        .map(|(block_index, block)| {
                            let (block_type, content) = match block {
                                ContentBlock::Text { text } => {
                                    ("text".to_string(), AnthropicBlockContent::Text { text })
                                }
                                ContentBlock::ToolUse { id, name, input } => (
                                    "tool_use".to_string(),
                                    AnthropicBlockContent::ToolUse { id, name, input },
                                ),
                                ContentBlock::ToolResult {
                                    tool_use_id,
                                    content,
                                    is_error,
                                } => (
                                    "tool_result".to_string(),
                                    AnthropicBlockContent::ToolResult {
                                        tool_use_id,
                                        content,
                                        is_error,
                                    },
                                ),
                                ContentBlock::Thinking {
                                    thinking,
                                    signature,
                                } => (
                                    "thinking".to_string(),
                                    AnthropicBlockContent::Thinking {
                                        thinking,
                                        signature,
                                    },
                                ),
                                ContentBlock::RedactedThinking { data } => {
                                    ("redacted_thinking".to_string(), {
                                        AnthropicBlockContent::RedactedThinking { data }
                                    })
                                }
                            };

                            Some(AnthropicContentBlock {
                                block_type,
                                content,
                                cache_control: if (old_cache_position == Some(msg_index)
                                    || new_cache_position == Some(msg_index))
                                    && block_index == 0
                                {
                                    Some(CacheControl {
                                        cache_type: "ephemeral".to_string(),
                                    })
                                } else {
                                    None
                                },
                            })
                        })
                        .filter_map(|x| x)
                        .collect(),
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

/// Cache state tracking for a specific content prefix
#[derive(Debug)]
struct CacheEntry {
    count: u32,
    last_used: SystemTime,
    current_cache_position: Option<usize>,
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

    // Customization points
    auth_provider: Box<dyn AuthProvider>,
    request_customizer: Box<dyn RequestCustomizer>,
    message_converter: Box<dyn MessageConverter>,
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
            auth_provider: Box::new(ApiKeyAuth::new(api_key)),
            request_customizer: Box::new(DefaultRequestCustomizer),
            message_converter: Box::new(DefaultMessageConverter::new()),
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
            auth_provider: Box::new(ApiKeyAuth::new(api_key)),
            request_customizer: Box::new(DefaultRequestCustomizer),
            message_converter: Box::new(DefaultMessageConverter::new()),
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
            auth_provider,
            request_customizer,
            message_converter,
        }
    }

    /// Set recorder for existing client
    pub fn with_recorder<P: AsRef<std::path::Path>>(mut self, recording_path: P) -> Self {
        self.recorder = Some(APIRecorder::new(recording_path));
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
                                                if let ContentBlock::ToolUse { name, id, .. } =
                                                    block
                                                {
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
                match process_chunk(
                    &chunk,
                    &mut line_buffer,
                    &mut blocks,
                    &mut usage,
                    &mut current_content,
                    callback,
                    &self.recorder,
                ) {
                    Ok(()) => continue,
                    Err(e) if e.to_string().contains("Tool limit reached") => {
                        debug!("Tool limit reached, stopping streaming early. Collected {} blocks so far", blocks.len());

                        // Finalize the current block with any accumulated content
                        if !blocks.is_empty() && !current_content.is_empty() {
                            match blocks.last_mut().unwrap() {
                                ContentBlock::Thinking { thinking, .. } => {
                                    *thinking = current_content.clone();
                                }
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
                    rate_limit_info: Some(crate::types::RateLimitInfo {
                        tokens_limit: rate_limits.tokens_limit,
                        tokens_remaining: rate_limits.tokens_remaining,
                    }),
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
                rate_limit_info: Some(crate::types::RateLimitInfo {
                    tokens_limit: rate_limits.tokens_limit,
                    tokens_remaining: rate_limits.tokens_remaining,
                }),
            };

            Ok((llm_response, rate_limits))
        }
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
        let (thinking, max_tokens) = if self.supports_thinking() {
            (
                Some(ThinkingConfiguration {
                    thinking_type: "enabled".to_string(),
                    budget_tokens: 4000,
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
            "temperature": 0.7,
            "system": system,
            "stream": streaming_callback.is_some(),
            "messages": messages_json,
        });

        if let Some(thinking_config) = thinking {
            anthropic_request["thinking"] = serde_json::to_value(thinking_config)?;
        }
        if let Some(tool_choice) = tool_choice {
            anthropic_request["tool_choice"] = tool_choice;
        }
        if let Some(tools) = tools {
            anthropic_request["tools"] = serde_json::to_value(tools)?;
        }

        // Allow request customizer to modify the request
        self.request_customizer
            .customize_request(&mut anthropic_request)?;

        self.send_with_retry(&anthropic_request, streaming_callback, 3)
            .await
    }
}
