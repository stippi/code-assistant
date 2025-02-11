#[cfg(test)]
mod tests;

pub mod anthropic;
pub mod ollama;
pub mod openai;
pub mod types;
pub mod vertex;

pub use anthropic::AnthropicClient;
pub use ollama::OllamaClient;
pub use openai::OpenAIClient;
pub use types::*;
pub use vertex::VertexClient;

use anyhow::Result;
use async_trait::async_trait;

pub type StreamingCallback = Box<dyn Fn(&str) -> Result<()> + Send + Sync>;

/// Trait for different LLM provider implementations
#[async_trait]
pub trait LLMProvider {
    /// Sends a request to the LLM service
    async fn send_message(
        &self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse>;
}
