//! AI Core client for Google Vertex AI / Gemini API
//!
//! This client wraps the VertexClient with AI Core authentication.

use crate::{
    auth::TokenManager,
    types::*,
    vertex::{AuthProvider, RequestCustomizer, VertexAuth, VertexClient},
    LLMProvider, StreamingCallback,
};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

// ============================================================================
// AI Core Authentication Provider for Vertex
// ============================================================================

/// AI Core authentication provider for Vertex API (uses Bearer token in headers)
struct AiCoreVertexAuthProvider {
    token_manager: Arc<TokenManager>,
}

impl AiCoreVertexAuthProvider {
    fn new(token_manager: Arc<TokenManager>) -> Self {
        Self { token_manager }
    }
}

#[async_trait]
impl AuthProvider for AiCoreVertexAuthProvider {
    async fn get_auth(&self) -> Result<VertexAuth> {
        let token = self.token_manager.get_valid_token().await?;
        Ok(VertexAuth {
            query_params: vec![], // AI Core doesn't use query params for auth
            headers: vec![("Authorization".to_string(), format!("Bearer {token}"))],
        })
    }
}

// ============================================================================
// AI Core Request Customizer for Vertex
// ============================================================================

/// AI Core request customizer for Vertex API
struct AiCoreVertexRequestCustomizer;

impl RequestCustomizer for AiCoreVertexRequestCustomizer {
    fn customize_request(&self, _request: &mut serde_json::Value) -> Result<()> {
        Ok(())
    }

    fn get_additional_headers(&self) -> Vec<(String, String)> {
        vec![
            ("AI-Resource-Group".to_string(), "default".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]
    }

    fn customize_url(&self, base_url: &str, model: &str, streaming: bool) -> String {
        // AI Core Vertex deployments use the same URL pattern
        if streaming {
            format!("{}/models/{}:streamGenerateContent", base_url, model)
        } else {
            format!("{}/models/{}:generateContent", base_url, model)
        }
    }
}

// ============================================================================
// AI Core Vertex Client
// ============================================================================

/// AI Core client for Google Vertex AI / Gemini models
pub struct AiCoreVertexClient {
    vertex_client: VertexClient,
    custom_config: Option<serde_json::Value>,
}

impl AiCoreVertexClient {
    fn create_vertex_client(
        token_manager: Arc<TokenManager>,
        base_url: String,
        model: String,
    ) -> VertexClient {
        let auth_provider = Box::new(AiCoreVertexAuthProvider::new(token_manager));
        let request_customizer = Box::new(AiCoreVertexRequestCustomizer);

        VertexClient::with_customization(model, base_url, auth_provider, request_customizer)
    }

    pub fn new(token_manager: Arc<TokenManager>, base_url: String, model: String) -> Self {
        let vertex_client = Self::create_vertex_client(token_manager, base_url, model);
        Self {
            vertex_client,
            custom_config: None,
        }
    }

    /// Create a new client with recording capability
    pub fn new_with_recorder<P: AsRef<std::path::Path>>(
        token_manager: Arc<TokenManager>,
        base_url: String,
        model: String,
        recording_path: P,
    ) -> Self {
        let vertex_client = Self::create_vertex_client(token_manager, base_url, model)
            .with_recorder(recording_path);
        Self {
            vertex_client,
            custom_config: None,
        }
    }

    /// Set custom model configuration to be merged into API requests
    pub fn with_custom_config(mut self, custom_config: serde_json::Value) -> Self {
        self.vertex_client = self.vertex_client.with_custom_config(custom_config.clone());
        self.custom_config = Some(custom_config);
        self
    }
}

#[async_trait]
impl LLMProvider for AiCoreVertexClient {
    async fn send_message(
        &mut self,
        request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        // Delegate to the wrapped VertexClient
        self.vertex_client
            .send_message(request, streaming_callback)
            .await
    }
}
