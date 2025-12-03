//! AI Core client for OpenAI Chat Completions API
//!
//! This client wraps the OpenAI client with AI Core authentication.

use crate::{
    auth::TokenManager,
    openai::{AuthProvider, OpenAIClient, RequestCustomizer},
    types::*,
    LLMProvider, StreamingCallback,
};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

/// AI Core authentication provider for OpenAI-style API
struct AiCoreOpenAIAuthProvider {
    token_manager: Arc<TokenManager>,
}

impl AiCoreOpenAIAuthProvider {
    fn new(token_manager: Arc<TokenManager>) -> Self {
        Self { token_manager }
    }
}

#[async_trait]
impl AuthProvider for AiCoreOpenAIAuthProvider {
    async fn get_auth_headers(&self) -> Result<Vec<(String, String)>> {
        let token = self.token_manager.get_valid_token().await?;
        Ok(vec![(
            "Authorization".to_string(),
            format!("Bearer {token}"),
        )])
    }
}

/// AI Core request customizer for OpenAI Chat Completions API
struct AiCoreOpenAIRequestCustomizer;

impl RequestCustomizer for AiCoreOpenAIRequestCustomizer {
    fn customize_request(&self, _request: &mut serde_json::Value) -> Result<()> {
        // No additional customization needed for OpenAI-style requests
        Ok(())
    }

    fn get_additional_headers(&self) -> Vec<(String, String)> {
        vec![
            ("AI-Resource-Group".to_string(), "default".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]
    }

    fn customize_url(&self, base_url: &str, _streaming: bool) -> String {
        // AI Core uses /chat/completions endpoint for OpenAI-compatible models
        format!("{base_url}/chat/completions")
    }
}

/// AI Core client for OpenAI Chat Completions API
pub struct AiCoreOpenAIClient {
    openai_client: OpenAIClient,
    custom_config: Option<serde_json::Value>,
}

impl AiCoreOpenAIClient {
    fn create_openai_client(
        token_manager: Arc<TokenManager>,
        base_url: String,
        model_id: String,
    ) -> OpenAIClient {
        let auth_provider = Box::new(AiCoreOpenAIAuthProvider::new(token_manager));
        let request_customizer = Box::new(AiCoreOpenAIRequestCustomizer);

        OpenAIClient::with_customization(model_id, base_url, auth_provider, request_customizer)
    }

    pub fn new(token_manager: Arc<TokenManager>, base_url: String, model_id: String) -> Self {
        let openai_client = Self::create_openai_client(token_manager, base_url, model_id);
        Self {
            openai_client,
            custom_config: None,
        }
    }

    /// Create a new client with recording capability
    ///
    /// Note: Recording is not yet implemented for OpenAI client.
    /// This constructor exists for API consistency.
    pub fn new_with_recorder<P: AsRef<std::path::Path>>(
        token_manager: Arc<TokenManager>,
        base_url: String,
        model_id: String,
        _recording_path: P,
    ) -> Self {
        // TODO: Add recording support to OpenAIClient
        Self::new(token_manager, base_url, model_id)
    }

    /// Set custom model configuration to be merged into API requests
    pub fn with_custom_config(mut self, custom_config: serde_json::Value) -> Self {
        self.openai_client = self.openai_client.with_custom_config(custom_config.clone());
        self.custom_config = Some(custom_config);
        self
    }
}

#[async_trait]
impl LLMProvider for AiCoreOpenAIClient {
    async fn send_message(
        &mut self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        // Delegate to the wrapped OpenAIClient
        self.openai_client
            .send_message(request, streaming_callback)
            .await
    }
}
