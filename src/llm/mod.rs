pub mod anthropic;
pub mod ollama;
pub mod openai;
pub mod types;
pub mod vertex;

pub use anthropic::AnthropicClient;
pub use ollama::OllamaClient;
pub use openai::OpenAIClient;
pub use vertex::VertexClient;
pub use types::*;

use anyhow::Result;
use async_trait::async_trait;

/// Trait for different LLM provider implementations
#[async_trait]
pub trait LLMProvider {
    /// Sends a request to the LLM service
    async fn send_message(&self, request: LLMRequest) -> Result<LLMResponse>;
}
