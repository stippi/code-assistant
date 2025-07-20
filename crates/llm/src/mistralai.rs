use super::openai::OpenAIClient;
use crate::{types::*, LLMProvider, StreamingCallback};
use anyhow::Result;
use async_trait::async_trait;

pub struct MistralAiClient {
    inner: OpenAIClient,
}

impl MistralAiClient {
    pub fn default_base_url() -> String {
        "https://api.mistral.ai/v1".to_string()
    }

    pub fn new(api_key: String, model: String, base_url: String) -> Self {
        Self {
            inner: OpenAIClient::new(api_key, model, base_url),
        }
    }
}

#[async_trait]
impl LLMProvider for MistralAiClient {
    async fn send_message(
        &mut self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        // Delegate to inner OpenAI client since the APIs are compatible
        self.inner.send_message(request, streaming_callback).await
    }
}
