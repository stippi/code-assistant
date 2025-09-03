use reqwest::Response;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};

/// Tracks token usage for a request/response pair
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone, Default)]
pub struct Usage {
    /// Number of tokens in the input (prompt)
    pub input_tokens: u32,
    /// Number of tokens in the output (completion)
    pub output_tokens: u32,
    /// Number of tokens written to cache
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
    /// Number of tokens read from cache
    #[serde(default)]
    pub cache_read_input_tokens: u32,
}

impl Usage {
    pub fn zero() -> Self {
        Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Generic request structure that can be mapped to different providers
#[derive(Debug, Clone)]
pub struct LLMRequest {
    pub messages: Vec<Message>,
    pub system_prompt: String,
    pub tools: Option<Vec<ToolDefinition>>,
    /// Custom text sequences that will cause the model to stop generating
    pub stop_sequences: Option<Vec<String>>,
    /// Request ID for consistent tool ID generation across providers
    pub request_id: u64,
    /// Session ID, for example used by OpenAI provider to optimize caching
    pub session_id: String,
}

impl Default for LLMRequest {
    fn default() -> Self {
        Self {
            messages: Vec::new(),
            system_prompt: String::new(),
            tools: None,
            stop_sequences: None,
            request_id: 1,
            session_id: "".to_string(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Message {
    pub role: MessageRole,
    pub content: MessageContent,
    /// Request ID for assistant messages (used for consistent tool ID generation)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<u64>,
    /// Token usage for assistant messages (tracks context size and costs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Structured(Vec<ContentBlock>),
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        signature: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        start_time: Option<SystemTime>,
        #[serde(skip_serializing_if = "Option::is_none")]
        end_time: Option<SystemTime>,
    },

    #[serde(rename = "redacted_thinking")]
    RedactedThinking {
        id: String,
        summary: Vec<serde_json::Value>,
        data: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        start_time: Option<SystemTime>,
        #[serde(skip_serializing_if = "Option::is_none")]
        end_time: Option<SystemTime>,
    },

    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        start_time: Option<SystemTime>,
        #[serde(skip_serializing_if = "Option::is_none")]
        end_time: Option<SystemTime>,
    },

    #[serde(rename = "image")]
    Image {
        /// Image format (e.g., "image/jpeg", "image/png")
        media_type: String,
        /// Base64-encoded image data
        data: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        start_time: Option<SystemTime>,
        #[serde(skip_serializing_if = "Option::is_none")]
        end_time: Option<SystemTime>,
    },

    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        start_time: Option<SystemTime>,
        #[serde(skip_serializing_if = "Option::is_none")]
        end_time: Option<SystemTime>,
    },

    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        start_time: Option<SystemTime>,
        #[serde(skip_serializing_if = "Option::is_none")]
        end_time: Option<SystemTime>,
    },
}

/// Rate limit information extracted from LLM provider headers
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct RateLimitInfo {
    /// Maximum tokens per minute/request limit
    pub tokens_limit: Option<u32>,
    /// Remaining tokens in current window
    pub tokens_remaining: Option<u32>,
}

/// Generic response structure
#[derive(Debug, Deserialize, Clone, Default)]
pub struct LLMResponse {
    pub content: Vec<ContentBlock>,
    pub usage: Usage,
    /// Rate limit information from provider headers
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit_info: Option<RateLimitInfo>,
}

impl LLMResponse {
    /// Distribute timestamps across all content blocks in the response
    /// For non-streaming responses where we have request start and response end times
    pub fn set_distributed_timestamps(
        &mut self,
        request_start: SystemTime,
        response_end: SystemTime,
    ) {
        if self.content.is_empty() {
            return;
        }

        let total_duration = response_end
            .duration_since(request_start)
            .unwrap_or(Duration::from_millis(0));
        let num_blocks = self.content.len();

        if num_blocks == 1 {
            // Single block gets the full duration
            if let Some(block) = self.content.first_mut() {
                block.set_timestamps(request_start, response_end);
            }
        } else {
            // Distribute time evenly across blocks
            let duration_per_block = total_duration / num_blocks as u32;

            for (i, block) in self.content.iter_mut().enumerate() {
                let block_start = request_start + (duration_per_block * i as u32);
                let block_end = if i == num_blocks - 1 {
                    // Last block ends at response_end to account for any rounding
                    response_end
                } else {
                    block_start + duration_per_block
                };

                block.set_timestamps(block_start, block_end);
            }
        }
    }
}

/// Common error types for all LLM providers
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("Rate limit exceeded: {0}")]
    RateLimit(String),

    #[error("Authentication failed: {0}")]
    Authentication(String),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Service error: {0}")]
    ServiceError(String),

    #[error("Service overloaded: {0}")]
    Overloaded(String),

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("Unknown error: {0}")]
    Unknown(String),
}

/// Context wrapper for API errors that includes rate limit information
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct ApiErrorContext<T> {
    pub error: ApiError,
    pub rate_limits: Option<T>,
}

/// Base trait for rate limit information
pub trait RateLimitHandler: Sized {
    /// Create a new instance from response headers
    fn from_response(response: &Response) -> Self;

    /// Get the delay duration before the next retry
    fn get_retry_delay(&self) -> Duration;

    /// Log the current rate limit status
    fn log_status(&self);
}

impl ContentBlock {
    /// Create a thinking content block from a String
    pub fn new_thinking(text: impl Into<String>, signature: impl Into<String>) -> Self {
        ContentBlock::Thinking {
            thinking: text.into(),
            signature: signature.into(),
            start_time: None,
            end_time: None,
        }
    }

    /// Create a text content block from a String
    pub fn new_text(text: impl Into<String>) -> Self {
        ContentBlock::Text {
            text: text.into(),
            start_time: None,
            end_time: None,
        }
    }

    /// Create an image content block from raw image data
    pub fn new_image(media_type: impl Into<String>, data: impl AsRef<[u8]>) -> Self {
        use base64::Engine as _;
        ContentBlock::Image {
            media_type: media_type.into(),
            data: base64::engine::general_purpose::STANDARD.encode(data.as_ref()),
            start_time: None,
            end_time: None,
        }
    }

    /// Create an image content block from base64-encoded data
    pub fn new_image_base64(media_type: impl Into<String>, base64_data: impl Into<String>) -> Self {
        ContentBlock::Image {
            media_type: media_type.into(),
            data: base64_data.into(),
            start_time: None,
            end_time: None,
        }
    }

    pub fn new_tool_use(
        id: impl Into<String>,
        name: impl Into<String>,
        input: impl Into<serde_json::Value>,
    ) -> Self {
        ContentBlock::ToolUse {
            id: id.into(),
            name: name.into(),
            input: input.into(),
            start_time: None,
            end_time: None,
        }
    }

    pub fn new_tool_result(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        ContentBlock::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error: None,
            start_time: None,
            end_time: None,
        }
    }

    pub fn new_error_tool_result(
        tool_use_id: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        ContentBlock::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error: Some(true),
            start_time: None,
            end_time: None,
        }
    }

    /// Get the start time of this content block
    pub fn start_time(&self) -> Option<SystemTime> {
        match self {
            ContentBlock::Thinking { start_time, .. } => *start_time,
            ContentBlock::RedactedThinking { start_time, .. } => *start_time,
            ContentBlock::Text { start_time, .. } => *start_time,
            ContentBlock::Image { start_time, .. } => *start_time,
            ContentBlock::ToolUse { start_time, .. } => *start_time,
            ContentBlock::ToolResult { start_time, .. } => *start_time,
        }
    }

    /// Get the end time of this content block
    pub fn end_time(&self) -> Option<SystemTime> {
        match self {
            ContentBlock::Thinking { end_time, .. } => *end_time,
            ContentBlock::RedactedThinking { end_time, .. } => *end_time,
            ContentBlock::Text { end_time, .. } => *end_time,
            ContentBlock::Image { end_time, .. } => *end_time,
            ContentBlock::ToolUse { end_time, .. } => *end_time,
            ContentBlock::ToolResult { end_time, .. } => *end_time,
        }
    }

    /// Set the start time of this content block
    pub fn set_start_time(&mut self, time: SystemTime) {
        match self {
            ContentBlock::Thinking { start_time, .. } => *start_time = Some(time),
            ContentBlock::RedactedThinking { start_time, .. } => *start_time = Some(time),
            ContentBlock::Text { start_time, .. } => *start_time = Some(time),
            ContentBlock::Image { start_time, .. } => *start_time = Some(time),
            ContentBlock::ToolUse { start_time, .. } => *start_time = Some(time),
            ContentBlock::ToolResult { start_time, .. } => *start_time = Some(time),
        }
    }

    /// Set the end time of this content block
    pub fn set_end_time(&mut self, time: SystemTime) {
        match self {
            ContentBlock::Thinking { end_time, .. } => *end_time = Some(time),
            ContentBlock::RedactedThinking { end_time, .. } => *end_time = Some(time),
            ContentBlock::Text { end_time, .. } => *end_time = Some(time),
            ContentBlock::Image { end_time, .. } => *end_time = Some(time),
            ContentBlock::ToolUse { end_time, .. } => *end_time = Some(time),
            ContentBlock::ToolResult { end_time, .. } => *end_time = Some(time),
        }
    }

    /// Set both start and end times of this content block
    pub fn set_timestamps(&mut self, start_time: SystemTime, end_time: SystemTime) {
        self.set_start_time(start_time);
        self.set_end_time(end_time);
    }

    /// Get the duration of this content block if both timestamps are available
    pub fn duration(&self) -> Option<Duration> {
        match (self.start_time(), self.end_time()) {
            (Some(start), Some(end)) => end.duration_since(start).ok(),
            _ => None,
        }
    }
}
