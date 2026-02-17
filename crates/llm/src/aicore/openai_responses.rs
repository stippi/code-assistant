//! AI Core client for OpenAI Responses API
//!
//! This client wraps the OpenAI Responses API client with AI Core authentication.
//! The Responses API is OpenAI's modern API format supporting:
//! - Stateless mode with encrypted reasoning
//! - Function calling
//! - Streaming with SSE

use crate::{
    auth::TokenManager,
    openai_responses::{AuthProvider, OpenAIResponsesClient, RequestCustomizer},
    types::*,
    LLMProvider, StreamingCallback,
};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

// ============================================================================
// AI Core Authentication Provider for OpenAI Responses API
// ============================================================================

/// AI Core authentication provider for OpenAI Responses API
struct AiCoreOpenAIResponsesAuthProvider {
    token_manager: Arc<TokenManager>,
}

impl AiCoreOpenAIResponsesAuthProvider {
    fn new(token_manager: Arc<TokenManager>) -> Self {
        Self { token_manager }
    }
}

#[async_trait]
impl AuthProvider for AiCoreOpenAIResponsesAuthProvider {
    async fn get_auth_headers(&self) -> Result<Vec<(String, String)>> {
        let token = self.token_manager.get_valid_token().await?;
        Ok(vec![(
            "Authorization".to_string(),
            format!("Bearer {token}"),
        )])
    }
}

// ============================================================================
// AI Core Request Customizer for OpenAI Responses API
// ============================================================================

/// AI Core request customizer for OpenAI Responses API
struct AiCoreOpenAIResponsesRequestCustomizer;

impl RequestCustomizer for AiCoreOpenAIResponsesRequestCustomizer {
    fn customize_request(&self, _request: &mut serde_json::Value) -> Result<()> {
        // No additional customization needed for Responses API requests
        Ok(())
    }

    fn get_additional_headers(&self) -> Vec<(String, String)> {
        vec![
            ("AI-Resource-Group".to_string(), "default".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]
    }

    fn customize_url(&self, base_url: &str, _streaming: bool) -> String {
        // AI Core uses /responses endpoint for OpenAI Responses API
        format!("{base_url}/responses")
    }
}

// ============================================================================
// AI Core OpenAI Responses Client
// ============================================================================

/// AI Core client for OpenAI Responses API
///
/// This client provides access to OpenAI's Responses API through AI Core,
/// supporting features like encrypted reasoning for stateless mode,
/// function calling, and streaming.
pub struct AiCoreOpenAIResponsesClient {
    responses_client: OpenAIResponsesClient,
    custom_config: Option<serde_json::Value>,
}

impl AiCoreOpenAIResponsesClient {
    fn create_responses_client(
        token_manager: Arc<TokenManager>,
        base_url: String,
        model_id: String,
    ) -> OpenAIResponsesClient {
        let auth_provider = Box::new(AiCoreOpenAIResponsesAuthProvider::new(token_manager));
        let request_customizer = Box::new(AiCoreOpenAIResponsesRequestCustomizer);

        OpenAIResponsesClient::with_customization(
            model_id,
            base_url,
            auth_provider,
            request_customizer,
        )
    }

    pub fn new(token_manager: Arc<TokenManager>, base_url: String, model_id: String) -> Self {
        let responses_client = Self::create_responses_client(token_manager, base_url, model_id);
        Self {
            responses_client,
            custom_config: None,
        }
    }

    /// Create a new client with recording capability
    pub fn new_with_recorder<P: AsRef<std::path::Path>>(
        token_manager: Arc<TokenManager>,
        base_url: String,
        model_id: String,
        recording_path: P,
    ) -> Self {
        let responses_client = Self::create_responses_client(token_manager, base_url, model_id)
            .with_recorder(recording_path);
        Self {
            responses_client,
            custom_config: None,
        }
    }

    /// Set custom model configuration to be merged into API requests
    pub fn with_custom_config(mut self, custom_config: serde_json::Value) -> Self {
        self.responses_client = self
            .responses_client
            .with_custom_config(custom_config.clone());
        self.custom_config = Some(custom_config);
        self
    }
}

#[async_trait]
impl LLMProvider for AiCoreOpenAIResponsesClient {
    async fn send_message(
        &mut self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        // Delegate to the wrapped OpenAIResponsesClient
        self.responses_client
            .send_message(request, streaming_callback)
            .await
    }
}
