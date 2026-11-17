//! How often and how patiently a failed LLM request is sent again
//! (`retry.json`): rate limits, overloaded or unreachable servers, broken
//! streams and empty answers all follow this one policy.

use crate::provider_config::ConfigurationSystem;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::warn;

const FILE_NAME: &str = "retry.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RetryPolicy {
    /// Retries of one request before the run fails.
    pub max_retries: u32,
    /// Upper bound for the wait before a retry, also when the provider asks
    /// for a longer one; at least one second.
    pub max_delay_secs: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 5,
            max_delay_secs: 300,
        }
    }
}

impl RetryPolicy {
    pub fn path() -> PathBuf {
        ConfigurationSystem::config_file_path(FILE_NAME)
    }

    /// The policy from the config directory; a missing or malformed file
    /// yields the defaults.
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
        self.save_to(&Self::path())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("Failed to write {}", path.display()))
    }

    /// Whether another retry may follow `completed` ones.
    pub fn allows_retry(&self, completed: u32) -> bool {
        completed < self.max_retries
    }

    /// `delay` cut to the configured maximum. A maximum below one second
    /// counts as one, so retries never follow each other back to back.
    pub fn cap(&self, delay: Duration) -> Duration {
        delay.min(Duration::from_secs(self.max_delay_secs.max(1)))
    }

    /// Exponential backoff for the retry after `completed` ones: `base`,
    /// then doubled for each further retry, at most the configured maximum.
    pub fn backoff(&self, base: Duration, completed: u32) -> Duration {
        let factor = 2u32.saturating_pow(completed.min(31));
        self.cap(base.saturating_mul(factor))
    }
}

/// Context on an error the client sent again as often as the policy
/// allows; whoever receives it should not retry it once more.
#[derive(Debug, thiserror::Error)]
#[error("gave up after {retries} retries")]
pub struct RetriesExhausted {
    pub retries: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_limited_policy_stops_after_max_retries() {
        let policy = RetryPolicy {
            max_retries: 2,
            max_delay_secs: 60,
        };
        assert!(policy.allows_retry(0));
        assert!(policy.allows_retry(1));
        assert!(!policy.allows_retry(2));
    }

    #[test]
    fn backoff_doubles_up_to_the_maximum_delay() {
        let policy = RetryPolicy {
            max_delay_secs: 10,
            ..RetryPolicy::default()
        };
        let base = Duration::from_secs(2);
        assert_eq!(policy.backoff(base, 0), Duration::from_secs(2));
        assert_eq!(policy.backoff(base, 1), Duration::from_secs(4));
        assert_eq!(policy.backoff(base, 2), Duration::from_secs(8));
        assert_eq!(policy.backoff(base, 3), Duration::from_secs(10));
        assert_eq!(policy.backoff(base, 1000), Duration::from_secs(10));
    }

    #[test]
    fn a_requested_wait_is_capped() {
        let policy = RetryPolicy {
            max_delay_secs: 30,
            ..RetryPolicy::default()
        };
        assert_eq!(
            policy.cap(Duration::from_secs(3600)),
            Duration::from_secs(30)
        );
        assert_eq!(policy.cap(Duration::from_secs(5)), Duration::from_secs(5));
    }

    #[test]
    fn a_maximum_of_zero_still_waits_a_second() {
        let policy = RetryPolicy {
            max_delay_secs: 0,
            ..RetryPolicy::default()
        };
        assert_eq!(policy.cap(Duration::from_secs(60)), Duration::from_secs(1));
        assert_eq!(policy.cap(Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn missing_fields_take_the_defaults_and_the_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        assert_eq!(RetryPolicy::load_from(&path), RetryPolicy::default());

        std::fs::write(&path, r#"{"max_retries": 9}"#).unwrap();
        let loaded = RetryPolicy::load_from(&path);
        assert_eq!(loaded.max_retries, 9);
        assert_eq!(loaded.max_delay_secs, RetryPolicy::default().max_delay_secs);

        let policy = RetryPolicy {
            max_retries: 7,
            max_delay_secs: 42,
        };
        policy.save_to(&path).unwrap();
        assert_eq!(RetryPolicy::load_from(&path), policy);
    }
}
