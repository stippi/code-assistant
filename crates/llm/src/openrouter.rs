use super::openai::OpenAIClient;
use crate::{types::*, LLMProvider, StreamingCallback};
use anyhow::Result;
use async_trait::async_trait;

pub struct OpenRouterClient {
    inner: OpenAIClient,
}

impl OpenRouterClient {
    pub fn default_base_url() -> String {
        "https://openrouter.ai/api/v1".to_string()
    }

    pub fn new(api_key: String, model: String, base_url: String) -> Self {
        Self {
            inner: OpenAIClient::new(api_key, model, base_url),
        }
    }

    /// Set custom model configuration to be merged into API requests
    pub fn with_custom_config(mut self, custom_config: serde_json::Value) -> Self {
        self.inner = self.inner.with_custom_config(custom_config);
        self
    }
}

#[async_trait]
impl LLMProvider for OpenRouterClient {
    async fn send_message(
        &mut self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        // Delegate to inner OpenAI client since the APIs are compatible
        self.inner.send_message(request, streaming_callback).await
    }
}
