use super::openai::{ApiKeyAuth, OpenAIClient, RequestCustomizer};
use crate::{types::*, LLMProvider, StreamingCallback};
use anyhow::Result;
use async_trait::async_trait;

/// Custom request customizer for Groq API that removes prompt_cache_key
pub struct GroqRequestCustomizer;

impl RequestCustomizer for GroqRequestCustomizer {
    fn customize_request(&self, request: &mut serde_json::Value) -> Result<()> {
        // Remove prompt_cache_key field if it exists
        if let Some(obj) = request.as_object_mut() {
            obj.remove("prompt_cache_key");
        }
        Ok(())
    }

    fn get_additional_headers(&self) -> Vec<(String, String)> {
        vec![("Content-Type".to_string(), "application/json".to_string())]
    }

    fn customize_url(&self, base_url: &str, _streaming: bool) -> String {
        format!("{base_url}/chat/completions")
    }
}

pub struct GroqClient {
    inner: OpenAIClient,
}

impl GroqClient {
    pub fn default_base_url() -> String {
        "https://api.groq.com/openai/v1".to_string()
    }

    pub fn new(api_key: String, model: String, base_url: String) -> Self {
        Self {
            inner: OpenAIClient::with_customization(
                model,
                base_url,
                Box::new(ApiKeyAuth::new(api_key)),
                Box::new(GroqRequestCustomizer),
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
impl LLMProvider for GroqClient {
    async fn send_message(
        &mut self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        // Delegate to inner OpenAI client since the APIs are compatible
        self.inner.send_message(request, streaming_callback).await
    }
}
