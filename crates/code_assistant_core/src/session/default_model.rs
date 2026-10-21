//! The model new sessions start with, shared by every frontend.
//!
//! Persisted at `<config_dir>/default-model.json` and read whenever a session
//! is created, so a change (e.g. in the GPUI settings) applies to the next
//! new session of every running frontend — including a long-lived ACP agent —
//! unless that frontend was started with `--model`.

use crate::utils::file_utils::atomic_write_json;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::warn;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DefaultModelConfig {
    /// The model's key (`provider/model-id`; older files hold its name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl DefaultModelConfig {
    /// Path of the config file in the resolved config directory.
    pub fn path() -> PathBuf {
        crate::config_dir::config_dir().join("default-model.json")
    }

    /// Load the config; a missing or malformed file yields no default.
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Self {
        let Ok(content) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&content).unwrap_or_else(|e| {
            warn!("Failed to parse {}: {e}; no default model", path.display());
            Self::default()
        })
    }

    /// Persist the config to the resolved config directory.
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        atomic_write_json(path, self)
    }
}

/// The key of the configured default model, if it is offered.
pub fn configured_default_model() -> Option<String> {
    let model = DefaultModelConfig::load().model?;
    let config = llm::provider_config::ConfigurationSystem::load().ok()?;
    config.resolve_key(&model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_has_no_default() {
        let dir = tempfile::tempdir().unwrap();
        let config = DefaultModelConfig::load_from(&dir.path().join("default-model.json"));
        assert_eq!(config.model, None);
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        temp_env::with_var("CODE_ASSISTANT_CONFIG_DIR", Some(dir.path()), || {
            let config = DefaultModelConfig {
                model: Some("Model A".into()),
            };
            config.save().unwrap();
            assert_eq!(DefaultModelConfig::load(), config);
        });
    }
}
