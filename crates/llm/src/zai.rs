use super::openai::{ApiKeyAuth, OpenAIClient, RequestCustomizer};
use crate::{types::*, LLMProvider, StreamingCallback};
use anyhow::Result;
use async_trait::async_trait;

/// Z.ai request customizer
/// TODO: Determine which fields need to be stripped (if any)
pub struct ZaiRequestCustomizer;

impl RequestCustomizer for ZaiRequestCustomizer {
    fn customize_request(&self, request: &mut serde_json::Value) -> Result<()> {
        // TODO: Uncomment and modify based on testing results
        // if let Some(obj) = request.as_object_mut() {
        //     // Remove unsupported fields
        //     obj.remove("stream_options");
        //     obj.remove("prompt_cache_key");
        // }
        let _ = request; // Silence unused warning for now
        Ok(())
    }

    fn get_additional_headers(&self) -> Vec<(String, String)> {
        vec![("Content-Type".to_string(), "application/json".to_string())]
    }

    fn customize_url(&self, base_url: &str, _streaming: bool) -> String {
        format!("{base_url}/chat/completions")
    }
}

pub struct ZaiClient {
    inner: OpenAIClient,
}

impl ZaiClient {
    pub fn default_base_url() -> String {
        "https://api.z.ai/api/paas/v4".to_string()
    }

    pub fn new(api_key: String, model: String, base_url: String) -> Self {
        let auth_provider = Box::new(ApiKeyAuth::new(api_key));
        let request_customizer = Box::new(ZaiRequestCustomizer);

        Self {
            inner: OpenAIClient::with_customization(
                model,
                base_url,
                auth_provider,
                request_customizer,
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
impl LLMProvider for ZaiClient {
    async fn send_message(
        &mut self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        // Delegate to inner OpenAI client since the APIs are compatible
        self.inner.send_message(request, streaming_callback).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zai_headers() {
        let customizer = ZaiRequestCustomizer;
        let headers = customizer.get_additional_headers();

        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "Content-Type");
        assert_eq!(headers[0].1, "application/json");
    }

    #[test]
    fn test_zai_url_customization() {
        let customizer = ZaiRequestCustomizer;
        let base_url = "https://api.z.ai/api/paas/v4";

        let url = customizer.customize_url(base_url, false);
        assert_eq!(url, "https://api.z.ai/api/paas/v4/chat/completions");

        let url_streaming = customizer.customize_url(base_url, true);
        assert_eq!(
            url_streaming,
            "https://api.z.ai/api/paas/v4/chat/completions"
        );
    }
}
