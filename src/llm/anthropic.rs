use crate::llm::{types::*, LLMProvider};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde::Serialize;

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

        let response = self
            .client
            .post(&self.base_url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2024-01-01") // We might want to make this configurable
            .json(&anthropic_request)
            .send()
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("Anthropic API error: {}", error_text);
        }

        let response = response.json().await?;
        Ok(response)
    }
}
