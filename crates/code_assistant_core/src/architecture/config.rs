//! Architecture overview settings, persisted at
//! `<config_dir>/architecture.json`.

use crate::utils::file_utils::atomic_write_json;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArchitectureConfig {
    /// The model explaining components (a key of `models.json`); empty
    /// uses the default model of new sessions.
    pub model: String,
    /// Explain the shown component with the model as soon as it is shown
    /// or changed; off, only the refresh button asks the model.
    pub auto_summarize: bool,
    /// Rescan when the project's files change.
    pub watch: bool,
}

impl Default for ArchitectureConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            auto_summarize: true,
            watch: true,
        }
    }
}

impl ArchitectureConfig {
    pub fn path() -> PathBuf {
        crate::config_dir::config_dir().join("architecture.json")
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

    /// The model to use: the configured one, else the default model.
    pub fn resolved_model(&self) -> Option<String> {
        if !self.model.is_empty() {
            return Some(self.model.clone());
        }
        crate::session::default_model::configured_default_model()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults_and_partial_files_merge() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("architecture.json");
        assert_eq!(
            ArchitectureConfig::load_from(&path),
            ArchitectureConfig::default()
        );
        std::fs::write(&path, r#"{"model": "Fast"}"#).unwrap();
        let config = ArchitectureConfig::load_from(&path);
        assert_eq!(config.model, "Fast");
        assert!(config.auto_summarize);
        assert_eq!(config.resolved_model().as_deref(), Some("Fast"));
    }
}
