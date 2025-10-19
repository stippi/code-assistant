use super::openai::{ApiKeyAuth, OpenAIClient, RequestCustomizer};
use crate::{types::*, LLMProvider, StreamingCallback};
use anyhow::Result;
use async_trait::async_trait;

/// Custom request customizer for Cerebras API that removes prompt_cache_key
pub struct CerebrasRequestCustomizer;

impl RequestCustomizer for CerebrasRequestCustomizer {
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

pub struct CerebrasClient {
    inner: OpenAIClient,
}

impl CerebrasClient {
    pub fn default_base_url() -> String {
        "https://api.cerebras.ai/v1".to_string()
    }

    pub fn new(api_key: String, model: String, base_url: String) -> Self {
        Self {
            inner: OpenAIClient::with_customization(
                model,
                base_url,
                Box::new(ApiKeyAuth::new(api_key)),
                Box::new(CerebrasRequestCustomizer),
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
impl LLMProvider for CerebrasClient {
    async fn send_message(
        &mut self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        // Delegate to inner OpenAI client with custom request processing
        self.inner.send_message(request, streaming_callback).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_cerebras_request_customizer_removes_prompt_cache_key() {
        let customizer = CerebrasRequestCustomizer;

        // Test request with prompt_cache_key
        let mut request = json!({
            "model": "gpt-oss-120b",
            "messages": [{"role": "user", "content": "Hello"}],
            "temperature": 0.7,
            "prompt_cache_key": "some-cache-key",
            "other_field": "should-remain"
        });

        // Apply customization
        customizer.customize_request(&mut request).unwrap();

        // Verify prompt_cache_key is removed
        assert!(request.get("prompt_cache_key").is_none());

        // Verify other fields remain
        assert_eq!(request.get("model").unwrap(), "gpt-oss-120b");
        assert_eq!(request.get("temperature").unwrap(), 0.7);
        assert_eq!(request.get("other_field").unwrap(), "should-remain");
    }

    #[test]
    fn test_cerebras_request_customizer_handles_missing_prompt_cache_key() {
        let customizer = CerebrasRequestCustomizer;

        // Test request without prompt_cache_key
        let mut request = json!({
            "model": "gpt-oss-120b",
            "messages": [{"role": "user", "content": "Hello"}],
            "temperature": 0.7
        });

        // Apply customization (should not fail)
        customizer.customize_request(&mut request).unwrap();

        // Verify request is unchanged
        assert_eq!(request.get("model").unwrap(), "gpt-oss-120b");
        assert_eq!(request.get("temperature").unwrap(), 0.7);
        assert!(request.get("prompt_cache_key").is_none());
    }

    #[test]
    fn test_cerebras_request_customizer_headers() {
        let customizer = CerebrasRequestCustomizer;
        let headers = customizer.get_additional_headers();

        assert_eq!(headers.len(), 1);
        assert_eq!(
            headers[0],
            ("Content-Type".to_string(), "application/json".to_string())
        );
    }

    #[test]
    fn test_cerebras_request_customizer_url() {
        let customizer = CerebrasRequestCustomizer;
        let base_url = "https://api.cerebras.ai/v1";

        let url = customizer.customize_url(base_url, false);
        assert_eq!(url, "https://api.cerebras.ai/v1/chat/completions");

        let url_streaming = customizer.customize_url(base_url, true);
        assert_eq!(url_streaming, "https://api.cerebras.ai/v1/chat/completions");
    }
}
