
use crate::llm::{
    types::*, LLMProvider, StreamingCallback,
};
use super::openai::OpenAIClient;
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
            inner: OpenAIClient::new(api_key, model, base_url)
        }
    }
}

#[async_trait]
impl LLMProvider for OpenRouterClient {
    async fn send_message(
        &self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        // Delegate to inner OpenAI client since the APIs are compatible
        self.inner.send_message(request, streaming_callback).await
    }
}
