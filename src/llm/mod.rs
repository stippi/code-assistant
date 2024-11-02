pub mod anthropic;
pub mod types;

pub use anthropic::AnthropicClient; // Den Client direkt verfügbar machen
pub use types::*; // Alle öffentlichen Typen verfügbar machen

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Trait for different LLM provider implementations
#[async_trait]
pub trait LLMProvider {
    /// Sends a request to the LLM service
    async fn send_message(&self, request: LLMRequest) -> Result<LLMResponse>;
}
