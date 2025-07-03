use reqwest::Response;
use serde::{Deserialize, Serialize};
use std::time::Duration;

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
#[derive(Debug, Clone, Default)]
pub struct LLMRequest {
    pub messages: Vec<Message>,
    pub system_prompt: String,
    pub tools: Option<Vec<ToolDefinition>>,
    /// Custom text sequences that will cause the model to stop generating
    pub stop_sequences: Option<Vec<String>>,
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
    Thinking { thinking: String, signature: String },

    #[serde(rename = "redacted_thinking")]
    RedactedThinking { data: String },

    #[serde(rename = "text")]
    Text { text: String },

    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },

    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
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

/// Streaming-specific errors that can occur during streaming callbacks
#[derive(Debug, thiserror::Error)]
pub enum StreamingError {
    #[error("Tool limit reached - only one tool per message allowed")]
    ToolLimitReached,

    #[error("Streaming cancelled by user")]
    UserCancelled,

    #[error("Streaming processor error: {0}")]
    ProcessorError(String),
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
