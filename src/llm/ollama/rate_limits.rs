use crate::llm::types::RateLimitHandler;
use reqwest::Response;
use std::time::Duration;
use tracing::debug;

/// Ollama doesn't provide rate limit headers, but we implement
/// basic rate limit handling for consistency with other providers
#[derive(Debug)]
pub struct OllamaRateLimitInfo {
    // Ollama runs locally and doesn't have rate limits, but we
    // implement the trait for consistency and potential future use
}

impl RateLimitHandler for OllamaRateLimitInfo {
    fn from_response(_response: &Response) -> Self {
        Self {}
    }

    fn get_retry_delay(&self) -> Duration {
        // Simple exponential backoff for network/service errors
        Duration::from_secs(2)
    }

    fn log_status(&self) {
        debug!("Ollama has no rate limits (running locally)");
    }
}
