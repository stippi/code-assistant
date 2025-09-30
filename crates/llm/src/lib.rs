//! LLM integration module providing abstraction over different LLM providers
//!
//! This module implements:
//! - Common interface for LLM interactions via the LLMProvider trait
//! - Support for multiple providers (Anthropic, OpenAI, Ollama, Vertex)
//! - Message streaming capabilities
//! - Provider-specific implementations and optimizations
//! - Shared types and utilities for LLM interactions
//! - Recording capabilities for debugging and testing

#[cfg(test)]
mod tests;

mod utils;

//pub mod aicore_converse;
pub mod aicore_invoke;
pub mod anthropic;
pub mod auth;
pub mod cerebras;
pub mod config;
pub mod display;
pub mod factory;
pub mod groq;
pub mod mistralai;
pub mod ollama;
pub mod openai;
pub mod openai_responses;
pub mod openrouter;
pub mod recording;
pub mod streaming;
pub mod types;
pub mod vertex;

pub use aicore_invoke::AiCoreClient;
pub use anthropic::AnthropicClient;
pub use cerebras::CerebrasClient;
pub use groq::GroqClient;
pub use mistralai::MistralAiClient;
pub use ollama::OllamaClient;
pub use openai::OpenAIClient;
pub use openai_responses::OpenAIResponsesClient;
pub use openrouter::OpenRouterClient;
pub use types::*;
pub use vertex::VertexClient;

use anyhow::Result;
use async_trait::async_trait;

/// Structure to represent different types of streaming content from LLMs
#[derive(Debug, Clone)]
pub enum StreamingChunk {
    /// Regular text content
    Text(String),
    /// Content identified as "thinking" (supported by some models)
    Thinking(String),
    /// JSON input for tool calls with optional metadata
    InputJson {
        content: String,
        tool_name: Option<String>,
        tool_id: Option<String>,
    },
    /// Rate limit notification with countdown in seconds
    RateLimit { seconds_remaining: u64 },
    /// Clear rate limit notification
    RateLimitClear,
    /// Indicates that streaming from the LLM has completed
    StreamingComplete,
    /// OpenAI reasoning summary started a new item
    ReasoningSummaryStart,
    /// OpenAI reasoning summary delta for the current item
    ReasoningSummaryDelta(String),
    /// Indicates reasoning block is complete
    ReasoningComplete,
}

pub type StreamingCallback = Box<dyn Fn(&StreamingChunk) -> Result<()> + Send + Sync>;

/// Trait for different LLM provider implementations
#[async_trait]
pub trait LLMProvider: Send + Sync {
    /// Sends a request to the LLM service
    async fn send_message(
        &mut self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse>;
}
