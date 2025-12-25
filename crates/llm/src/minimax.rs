use super::anthropic::{
    AnthropicClient, ApiKeyAuth, DefaultMessageConverter, DefaultRequestCustomizer,
};
use crate::{types::*, LLMProvider, StreamingCallback};
use anyhow::Result;
use async_trait::async_trait;

pub struct MinimaxClient {
    inner: AnthropicClient,
}

impl MinimaxClient {
    pub fn default_base_url() -> String {
        "https://api.minimax.io/anthropic".to_string()
    }

    pub fn new(api_key: String, model: String, base_url: String) -> Self {
        let auth_provider = Box::new(ApiKeyAuth::new(api_key));
        let request_customizer = Box::new(DefaultRequestCustomizer);
        let message_converter = Box::new(DefaultMessageConverter::new());

        Self {
            inner: AnthropicClient::with_customization(
                model,
                base_url,
                auth_provider,
                request_customizer,
                message_converter,
            ),
        }
    }

    /// Set custom model configuration to be merged into API requests
    pub fn with_custom_config(mut self, custom_config: serde_json::Value) -> Self {
        self.inner = self.inner.with_custom_config(custom_config);
        self
    }
}

#[async_trait]
impl LLMProvider for MinimaxClient {
    async fn send_message(
        &mut self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        // Delegate to inner Anthropic client since the APIs are compatible
        self.inner.send_message(request, streaming_callback).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_minimax_default_url() {
        assert_eq!(
            MinimaxClient::default_base_url(),
            "https://api.minimax.io/anthropic"
        );
    }
}
