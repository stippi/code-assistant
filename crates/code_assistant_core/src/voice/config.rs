//! Voice mode configuration, persisted at `<config_dir>/voice.json`.

use crate::utils::file_utils::atomic_write_json;
use anyhow::{Result, anyhow, bail};
use llm::provider_config::ConfigurationSystem;
use llm::realtime::RealtimeConnector;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

/// Which conversations report to the voice agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NotifyScope {
    /// Every conversation of this process.
    #[default]
    All,
    /// Only conversations the voice agent created or messaged.
    Touched,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceConfig {
    /// Provider id from `providers.json`; empty while unconfigured.
    pub provider: String,
    /// The realtime model; for an AI Core provider the key of its `models`
    /// map whose deployment serves voice mode.
    pub model: String,
    pub voice: String,
    /// WebSocket URL overriding the one derived from the provider's
    /// `base_url`, for compatible gateways.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Model transcribing the user's speech (for the transcript); empty
    /// turns transcription off.
    pub transcription_model: String,
    /// Semantic turn detection eagerness: `low`, `medium`, `high`, `auto`.
    pub vad_eagerness: String,
    /// Silence after the model spoke before notifications may speak.
    pub cooling_ms: u64,
    pub notify: NotifyScope,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            provider: String::new(),
            model: "gpt-realtime".into(),
            voice: "marin".into(),
            url: None,
            transcription_model: "gpt-4o-mini-transcribe".into(),
            vad_eagerness: "auto".into(),
            cooling_ms: 1500,
            notify: NotifyScope::All,
        }
    }
}

impl VoiceConfig {
    pub fn path() -> PathBuf {
        crate::config_dir::config_dir().join("voice.json")
    }

    /// Load the config; a missing or malformed file yields the defaults.
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Self {
        let Ok(content) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&content).unwrap_or_else(|e| {
            warn!("Failed to parse {}: {e}; using defaults", path.display());
            Self::default()
        })
    }

    pub fn save(&self) -> Result<()> {
        atomic_write_json(&Self::path(), self)
    }

    pub fn is_configured(&self) -> bool {
        !self.provider.is_empty() && !self.model.is_empty()
    }

    pub fn cooling(&self) -> Duration {
        Duration::from_millis(self.cooling_ms)
    }

    pub fn turn_detection(&self) -> Value {
        let eagerness = if self.vad_eagerness.is_empty() {
            "auto"
        } else {
            &self.vad_eagerness
        };
        // The voice agent handles barge-in itself (cancel, truncate), so the
        // server only detects turns and answers them.
        json!({
            "type": "semantic_vad",
            "eagerness": eagerness,
            "create_response": true,
            "interrupt_response": false,
        })
    }

    pub fn transcription(&self) -> Option<String> {
        (!self.transcription_model.is_empty()).then(|| self.transcription_model.clone())
    }

    /// The connector for the configured provider in `providers.json`.
    pub fn connector(&self) -> Result<Arc<dyn RealtimeConnector>> {
        if !self.is_configured() {
            bail!("Voice mode is not configured: choose a provider and model in Settings → Voice");
        }
        let providers = ConfigurationSystem::load_providers_config(None)?;
        let provider = providers
            .get(&self.provider)
            .ok_or_else(|| anyhow!("Unknown provider '{}' in voice.json", self.provider))?;
        llm::realtime::connector_for_provider(provider, &self.model, self.url.as_deref())
    }
}

pub use llm::realtime::supports_realtime;

#[cfg(test)]
mod tests {
    use super::*;
    use llm::provider_config::ProviderConfig;

    fn provider(kind: &str, config: Value) -> ProviderConfig {
        ProviderConfig {
            label: "p".into(),
            provider: kind.into(),
            config,
        }
    }

    #[test]
    fn openai_and_ai_core_providers_serve_realtime() {
        assert!(supports_realtime(&provider("openai-responses", json!({}))));
        assert!(supports_realtime(&provider("ai-core", json!({}))));
        assert!(!supports_realtime(&provider("anthropic", json!({}))));
    }

    #[test]
    fn a_chatgpt_subscription_login_serves_no_realtime() {
        let p = provider("openai-responses-ws", json!({"codex_auth": true}));
        assert!(!supports_realtime(&p));
        let err = llm::realtime::connector_for_provider(&p, "gpt-realtime", None)
            .err()
            .unwrap();
        assert!(err.to_string().contains("ChatGPT subscription"));
    }

    #[test]
    fn ai_core_needs_a_deployment_for_the_voice_model() {
        let p = provider(
            "ai-core",
            json!({
                "client_id": "id", "client_secret": "s", "token_url": "https://t",
                "api_base_url": "https://api.example/v2/inference",
                "models": {"gpt-realtime": {"deployment": "d1", "api_type": "openai"}}
            }),
        );
        let connector = llm::realtime::connector_for_provider(&p, "gpt-realtime", None).unwrap();
        assert!(!connector.declares_model());
        let missing = llm::realtime::connector_for_provider(&p, "other", None);
        assert!(missing.err().unwrap().to_string().contains("no deployment"));
    }

    #[test]
    fn non_realtime_providers_are_rejected() {
        let p = provider("anthropic", json!({"api_key": "k"}));
        assert!(llm::realtime::connector_for_provider(&p, "m", None).is_err());
    }

    #[test]
    fn missing_file_yields_defaults_and_partial_files_merge() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            VoiceConfig::load_from(&dir.path().join("none.json")),
            VoiceConfig::default()
        );
        let path = dir.path().join("voice.json");
        std::fs::write(&path, r#"{"provider":"openai","notify":"touched"}"#).unwrap();
        let config = VoiceConfig::load_from(&path);
        assert_eq!(config.provider, "openai");
        assert_eq!(config.notify, NotifyScope::Touched);
        assert_eq!(config.model, "gpt-realtime");
    }
}
