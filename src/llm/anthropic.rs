use crate::llm::{types::*, LLMProvider};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde::Serialize;
use std::time::Duration;
use tokio::time::sleep;
use tracing::debug;

/// Anthropic-specific request structure
#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    messages: Vec<Message>,
    max_tokens: usize,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
}

pub struct AnthropicClient {
    client: Client,
    api_key: String,
    base_url: String,
    model: String,
}

impl AnthropicClient {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url: "https://api.anthropic.com/v1/messages".to_string(),
            model,
        }
    }

    async fn send_with_retry(
        &self,
        request: &AnthropicRequest,
        max_retries: u32,
    ) -> Result<LLMResponse> {
        let mut attempts = 0;
        let base_delay = Duration::from_secs(2);

        loop {
            match self.try_send_request(request).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    if Self::is_rate_limit_error(&e) && attempts < max_retries {
                        attempts += 1;
                        let delay = base_delay * 2u32.pow(attempts - 1); // Exponential backoff
                        debug!("Rate limit hit, retrying in {} seconds", delay.as_secs());
                        sleep(delay).await;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
    }

    fn is_rate_limit_error(error: &anyhow::Error) -> bool {
        error.to_string().contains("rate limit") // Adjust based on actual API error response
    }

    async fn try_send_request(&self, request: &AnthropicRequest) -> Result<LLMResponse> {
        let response = self
            .client
            .post(&self.base_url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(request)
            .send()
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("Anthropic API error: {}", error_text);
        }

        Ok(response.json().await?)
    }
}

#[async_trait]
impl LLMProvider for AnthropicClient {
    async fn send_message(&self, request: LLMRequest) -> Result<LLMResponse> {
        let anthropic_request = AnthropicRequest {
            model: self.model.clone(),
            messages: request.messages,
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            system: request.system_prompt,
        };

        self.send_with_retry(&anthropic_request, 3).await
    }
}
