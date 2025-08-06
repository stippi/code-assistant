use crate::{
    anthropic::{AnthropicClient, AuthProvider, DefaultMessageConverter, RequestCustomizer},
    types::*,
    LLMProvider, StreamingCallback,
};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

use super::auth::TokenManager;

/// AiCore authentication provider using TokenManager
pub struct AiCoreAuthProvider {
    token_manager: Arc<TokenManager>,
}

impl AiCoreAuthProvider {
    pub fn new(token_manager: Arc<TokenManager>) -> Self {
        Self { token_manager }
    }
}

#[async_trait]
impl AuthProvider for AiCoreAuthProvider {
    async fn get_auth_headers(&self) -> Result<Vec<(String, String)>> {
        let token = self.token_manager.get_valid_token().await?;
        Ok(vec![(
            "Authorization".to_string(),
            format!("Bearer {token}"),
        )])
    }
}

/// AiCore request customizer
pub struct AiCoreRequestCustomizer;

impl RequestCustomizer for AiCoreRequestCustomizer {
    fn customize_request(&self, request: &mut serde_json::Value) -> Result<()> {
        if let Value::Object(ref mut map) = request {
            // Remove stream and model fields after URL routing is done
            map.remove("stream");
            map.remove("model");
            // Add anthropic_version for AiCore
            map.insert(
                "anthropic_version".to_string(),
                Value::String("bedrock-2023-05-31".to_string()),
            );
        }
        Ok(())
    }

    fn get_additional_headers(&self) -> Vec<(String, String)> {
        vec![
            ("AI-Resource-Group".to_string(), "default".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
            (
                "anthropic-beta".to_string(),
                "output-128k-2025-02-19".to_string(),
            ),
        ]
    }

    fn customize_url(&self, base_url: &str, streaming: bool) -> String {
        if streaming {
            format!("{base_url}/invoke-with-response-stream")
        } else {
            format!("{base_url}/invoke")
        }
    }
}

pub struct AiCoreClient {
    anthropic_client: AnthropicClient,
}

impl AiCoreClient {
    fn create_anthropic_client(
        token_manager: Arc<TokenManager>,
        base_url: String,
    ) -> AnthropicClient {
        let auth_provider = Box::new(AiCoreAuthProvider::new(token_manager));
        let request_customizer = Box::new(AiCoreRequestCustomizer);
        let message_converter = Box::new(DefaultMessageConverter::new());

        AnthropicClient::with_customization(
            "ignored".to_string(), // Default model, can be overridden
            base_url,
            auth_provider,
            request_customizer,
            message_converter,
        )
    }

    pub fn new(token_manager: Arc<TokenManager>, base_url: String) -> Self {
        let anthropic_client = Self::create_anthropic_client(token_manager, base_url);
        Self { anthropic_client }
    }

    /// Create a new client with recording capability
    pub fn new_with_recorder<P: AsRef<std::path::Path>>(
        token_manager: Arc<TokenManager>,
        base_url: String,
        recording_path: P,
    ) -> Self {
        let anthropic_client =
            Self::create_anthropic_client(token_manager, base_url).with_recorder(recording_path);

        Self { anthropic_client }
    }
}

#[async_trait]
impl LLMProvider for AiCoreClient {
    async fn send_message(
        &mut self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        // Delegate to the wrapped AnthropicClient
        self.anthropic_client
            .send_message(request, streaming_callback)
            .await
    }
}
