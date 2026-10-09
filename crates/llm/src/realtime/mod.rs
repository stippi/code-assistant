//! Realtime (speech-to-speech) model sessions over the OpenAI Realtime
//! protocol.
//!
//! A realtime session is one long-lived WebSocket: audio streams both ways,
//! the server detects turns and keeps the conversation state. This module
//! only moves typed events; deciding *when* to send what is the caller's job
//! (see the voice agent in `code_assistant_core::voice`).
//!
//! [`RealtimeConnector`] is the seam: the WebSocket implementation is
//! [`WsConnector`], tests hand out channel pairs of their own.

mod aicore;
mod events;
mod transport;

pub use aicore::AiCoreConnector;
pub use events::{
    ClientEvent, ConversationItem, ErrorInfo, ResponseInfo, SAMPLE_RATE, ServerEvent,
    SessionSettings, ToolDefinition, decode_pcm16, encode_pcm16,
};
pub use transport::{RealtimeEndpoint, WsConnector};

use crate::provider_config::ProviderConfig;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;

/// What arrives from an open session.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Event(ServerEvent),
    /// The connection ended; carries the reason when one is known. Nothing
    /// follows.
    Closed(Option<String>),
}

/// An open realtime session as a pair of channels. Dropping `outgoing`
/// closes the connection.
pub struct RealtimeConnection {
    pub outgoing: mpsc::UnboundedSender<ClientEvent>,
    pub incoming: mpsc::UnboundedReceiver<Incoming>,
}

/// Opens realtime sessions; called again to reconnect.
#[async_trait::async_trait]
pub trait RealtimeConnector: Send + Sync {
    async fn connect(&self) -> Result<RealtimeConnection>;

    /// Whether `session.update` names the model. A gateway that routes to a
    /// deployment (AI Core) knows the model already.
    fn declares_model(&self) -> bool {
        true
    }
}

const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Whether a provider can serve realtime sessions: OpenAI types with an API
/// key, and AI Core. A ChatGPT subscription login (`codex_auth`) cannot; its
/// token grants no access to the Realtime API.
pub fn supports_realtime(provider: &ProviderConfig) -> bool {
    match provider.provider.as_str() {
        "openai" | "openai-responses" | "openai-responses-ws" => !uses_codex_auth(provider),
        "ai-core" => true,
        _ => false,
    }
}

fn uses_codex_auth(provider: &ProviderConfig) -> bool {
    provider
        .config
        .get("codex_auth")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// The connector for `model` on a provider from `providers.json`.
///
/// - OpenAI types: `api_key` auth, URL derived from `base_url`.
/// - `ai-core`: client credentials; `model` must be a key of the provider's
///   `models` map, whose deployment serves the session.
///
/// `url_override` replaces the derived URL (OpenAI types only).
pub fn connector_for_provider(
    provider: &ProviderConfig,
    model: &str,
    url_override: Option<&str>,
) -> Result<Arc<dyn RealtimeConnector>> {
    let config = &provider.config;
    let text = |key: &str| -> Result<String> {
        config
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .with_context(|| format!("The provider '{}' has no {key}", provider.label))
    };
    match provider.provider.as_str() {
        "openai" | "openai-responses" | "openai-responses-ws" => {
            if uses_codex_auth(provider) {
                bail!(
                    "Provider '{}' signs in with a ChatGPT subscription, which has no access to \
                     the Realtime API; voice mode needs an OpenAI provider with an API key",
                    provider.label
                );
            }
            let api_key = text("api_key")?;
            let base_url = text("base_url").unwrap_or_else(|_| DEFAULT_OPENAI_BASE_URL.into());
            let mut endpoint = RealtimeEndpoint::openai(&base_url, &api_key, model);
            if let Some(url) = url_override.filter(|url| !url.is_empty()) {
                endpoint.url = url.to_string();
            }
            Ok(Arc::new(WsConnector::new(endpoint)))
        }
        "ai-core" => {
            let deployment = config
                .get("models")
                .and_then(|models| models.get(model))
                .ok_or_else(|| {
                    anyhow!(
                        "The AI Core provider '{}' has no deployment for model '{model}' \
                         (add it to the provider's models)",
                        provider.label
                    )
                })?;
            let deployment = crate::factory::parse_aicore_deployment(model, deployment)?;
            Ok(Arc::new(AiCoreConnector::new(
                text("client_id")?,
                text("client_secret")?,
                text("token_url")?,
                text("api_base_url")?,
                deployment.deployment_uuid,
            )))
        }
        other => bail!(
            "Provider '{}' ({other}) has no realtime API; voice mode needs an OpenAI or AI Core \
             provider",
            provider.label
        ),
    }
}
