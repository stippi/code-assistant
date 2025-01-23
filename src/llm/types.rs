use crate::types::ToolDefinition;
use reqwest::Response;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Generic request structure that can be mapped to different providers
#[derive(Debug)]
pub struct LLMRequest {
    pub messages: Vec<Message>,
    pub system_prompt: String,
    pub tools: Option<Vec<ToolDefinition>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: MessageContent,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Structured(Vec<ContentBlock>),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
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
    },
}

/// Generic response structure
#[derive(Debug, Deserialize)]
pub struct LLMResponse {
    pub content: Vec<ContentBlock>,
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
