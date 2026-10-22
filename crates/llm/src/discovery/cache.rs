//! The last model listing of every provider, persisted next to the
//! provider config (`model-discovery.json`).

use super::DiscoveredModel;
use crate::provider_config::ConfigurationSystem;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

const FILE_NAME: &str = "model-discovery.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryCache {
    /// Keyed by provider ID (a key of `providers.json`).
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderDiscovery>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDiscovery {
    /// When the listing was fetched (seconds since the Unix epoch).
    pub fetched_at: u64,
    #[serde(default)]
    pub models: Vec<DiscoveredModel>,
    /// Why the last listing failed; `models` then holds the previous result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl DiscoveryCache {
    pub fn path() -> PathBuf {
        ConfigurationSystem::config_file_path(FILE_NAME)
    }

    /// Load the cache; a missing or unreadable file yields an empty one.
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Self {
        let Ok(content) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&content).unwrap_or_else(|e| {
            warn!("Ignoring unreadable {}: {e}", path.display());
            Self::default()
        })
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// The listed model `model_id` of `provider_id`, if known.
    pub fn model(&self, provider_id: &str, model_id: &str) -> Option<&DiscoveredModel> {
        self.providers
            .get(provider_id)?
            .models
            .iter()
            .find(|model| model.id == model_id)
    }

    /// Record the outcome of listing `provider_id`'s models. A failure
    /// keeps the previously listed models.
    pub fn record(&mut self, provider_id: &str, result: Result<Vec<DiscoveredModel>>) {
        let entry = self.providers.entry(provider_id.to_string()).or_default();
        entry.fetched_at = now();
        match result {
            Ok(models) => {
                entry.models = models;
                entry.error = None;
            }
            Err(error) => entry.error = Some(format!("{error:#}")),
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_listing_keeps_the_previous_models() {
        let mut cache = DiscoveryCache::default();
        cache.record("anthropic", Ok(vec![DiscoveredModel::new("claude-opus-5")]));
        cache.record("anthropic", Err(anyhow::anyhow!("401 Unauthorized")));

        let entry = &cache.providers["anthropic"];
        assert_eq!(entry.models.len(), 1);
        assert_eq!(entry.error.as_deref(), Some("401 Unauthorized"));
        assert!(cache.model("anthropic", "claude-opus-5").is_some());
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let mut cache = DiscoveryCache::default();
        cache.record("ollama", Ok(vec![DiscoveredModel::new("qwen3.8:27b-mlx")]));
        cache.save_to(&path).unwrap();
        assert_eq!(DiscoveryCache::load_from(&path), cache);
        assert_eq!(
            DiscoveryCache::load_from(&dir.path().join("missing.json")),
            DiscoveryCache::default()
        );
    }
}
