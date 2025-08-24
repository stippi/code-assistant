use super::openai::{ApiKeyAuth, OpenAIClient, RequestCustomizer};
use crate::{types::*, LLMProvider, StreamingCallback};
use anyhow::Result;
use async_trait::async_trait;

/// MistralAI request customizer that strips unsupported fields
pub struct MistralAiRequestCustomizer;

impl RequestCustomizer for MistralAiRequestCustomizer {
    fn customize_request(&self, request: &mut serde_json::Value) -> Result<()> {
        if let Some(obj) = request.as_object_mut() {
            // Remove stream_options and prompt_cache_key field as MistralAI doesn't support them
            obj.remove("stream_options");
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

pub struct MistralAiClient {
    inner: OpenAIClient,
}

impl MistralAiClient {
    pub fn default_base_url() -> String {
        "https://api.mistral.ai/v1".to_string()
    }

    pub fn new(api_key: String, model: String, base_url: String) -> Self {
        let auth_provider = Box::new(ApiKeyAuth::new(api_key));
        let request_customizer = Box::new(MistralAiRequestCustomizer);

        Self {
            inner: OpenAIClient::with_customization(
                model,
                base_url,
                auth_provider,
                request_customizer,
            ),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_mistral_request_customization() {
        let customizer = MistralAiRequestCustomizer;

        // Create a request with stream_options (like OpenAI would send)
        let mut request = json!({
            "model": "mistral-large-latest",
            "messages": [
                {"role": "user", "content": "Hello"}
            ],
            "stream": true,
            "stream_options": {
                "include_usage": true
            },
            "temperature": 1.0
        });

        // Verify stream_options is present initially
        assert!(request.as_object().unwrap().contains_key("stream_options"));

        // Apply MistralAI customization
        customizer.customize_request(&mut request).unwrap();

        // Verify stream_options is removed
        assert!(!request.as_object().unwrap().contains_key("stream_options"));

        // Verify other fields are still present
        assert!(request.as_object().unwrap().contains_key("model"));
        assert!(request.as_object().unwrap().contains_key("messages"));
        assert!(request.as_object().unwrap().contains_key("stream"));
        assert!(request.as_object().unwrap().contains_key("temperature"));
    }

    #[test]
    fn test_mistral_headers() {
        let customizer = MistralAiRequestCustomizer;
        let headers = customizer.get_additional_headers();

        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "Content-Type");
        assert_eq!(headers[0].1, "application/json");
    }

    #[test]
    fn test_mistral_url_customization() {
        let customizer = MistralAiRequestCustomizer;
        let base_url = "https://api.mistral.ai/v1";

        let url = customizer.customize_url(base_url, false);
        assert_eq!(url, "https://api.mistral.ai/v1/chat/completions");

        let url_streaming = customizer.customize_url(base_url, true);
        assert_eq!(url_streaming, "https://api.mistral.ai/v1/chat/completions");
    }
}
