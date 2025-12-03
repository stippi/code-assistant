//! AI Core provider module
//!
//! AI Core acts as a proxy service that can route to different backend vendors.
//! This module provides support for multiple vendor API types:
//! - Anthropic (Claude models via Bedrock-style API)
//! - OpenAI (Chat Completions API)
//! - Vertex (Google Gemini API)

mod anthropic;
mod openai;
mod types;
mod vertex;

pub use anthropic::AiCoreAnthropicClient;
pub use openai::AiCoreOpenAIClient;
pub use types::AiCoreApiType;
pub use vertex::AiCoreVertexClient;

use crate::auth::TokenManager;
use crate::LLMProvider;
use std::path::Path;
use std::sync::Arc;

/// Create an AI Core client based on the API type
pub fn create_aicore_client(
    api_type: AiCoreApiType,
    token_manager: Arc<TokenManager>,
    base_url: String,
    model_id: String,
) -> Box<dyn LLMProvider> {
    match api_type {
        AiCoreApiType::Anthropic => Box::new(AiCoreAnthropicClient::new(token_manager, base_url)),
        AiCoreApiType::OpenAI => {
            Box::new(AiCoreOpenAIClient::new(token_manager, base_url, model_id))
        }
        AiCoreApiType::Vertex => {
            Box::new(AiCoreVertexClient::new(token_manager, base_url, model_id))
        }
    }
}

/// Create an AI Core client with recording capability
pub fn create_aicore_client_with_recorder<P: AsRef<Path>>(
    api_type: AiCoreApiType,
    token_manager: Arc<TokenManager>,
    base_url: String,
    model_id: String,
    recording_path: P,
) -> Box<dyn LLMProvider> {
    match api_type {
        AiCoreApiType::Anthropic => Box::new(AiCoreAnthropicClient::new_with_recorder(
            token_manager,
            base_url,
            recording_path,
        )),
        AiCoreApiType::OpenAI => Box::new(AiCoreOpenAIClient::new_with_recorder(
            token_manager,
            base_url,
            model_id,
            recording_path,
        )),
        AiCoreApiType::Vertex => Box::new(AiCoreVertexClient::new_with_recorder(
            token_manager,
            base_url,
            model_id,
            recording_path,
        )),
    }
}
