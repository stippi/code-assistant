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
    /// Send a failed request again until it succeeds or the user stops the
    /// run; `max_retries` is ignored then.
    pub retry_forever: bool,
    /// Retries of one request before the run fails.
    pub max_retries: u32,
    /// Upper bound for the wait before a retry, also when the provider asks
    /// for a longer one.
    pub max_delay_secs: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            retry_forever: false,
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
        self.retry_forever || completed < self.max_retries
    }

    /// The number of retries, `None` when there is no limit.
    pub fn max_attempts(&self) -> Option<u32> {
        (!self.retry_forever).then_some(self.max_retries)
    }

    /// `delay` cut to the configured maximum.
    pub fn cap(&self, delay: Duration) -> Duration {
        delay.min(Duration::from_secs(self.max_delay_secs))
    }

    /// Exponential backoff for the retry after `completed` ones: `base`,
    /// then doubled for each further retry, at most the configured maximum.
    pub fn backoff(&self, base: Duration, completed: u32) -> Duration {
        let factor = 2u32.saturating_pow(completed.min(31));
        self.cap(base.saturating_mul(factor))
    }
}

/// "3/5" for the third of five retries, "3" without a limit.
pub fn attempt_label(attempt: u32, max_attempts: Option<u32>) -> String {
    match max_attempts {
        Some(max) => format!("{attempt}/{max}"),
        None => attempt.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_limited_policy_stops_after_max_retries() {
        let policy = RetryPolicy {
            retry_forever: false,
            max_retries: 2,
            max_delay_secs: 60,
        };
        assert!(policy.allows_retry(0));
        assert!(policy.allows_retry(1));
        assert!(!policy.allows_retry(2));
        assert_eq!(policy.max_attempts(), Some(2));
    }

    #[test]
    fn retrying_forever_never_stops() {
        let policy = RetryPolicy {
            retry_forever: true,
            max_retries: 0,
            max_delay_secs: 60,
        };
        assert!(policy.allows_retry(0));
        assert!(policy.allows_retry(u32::MAX));
        assert_eq!(policy.max_attempts(), None);
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
    fn missing_fields_take_the_defaults_and_the_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        assert_eq!(RetryPolicy::load_from(&path), RetryPolicy::default());

        std::fs::write(&path, r#"{"retry_forever": true}"#).unwrap();
        let loaded = RetryPolicy::load_from(&path);
        assert!(loaded.retry_forever);
        assert_eq!(loaded.max_delay_secs, RetryPolicy::default().max_delay_secs);

        let policy = RetryPolicy {
            retry_forever: false,
            max_retries: 7,
            max_delay_secs: 42,
        };
        policy.save_to(&path).unwrap();
        assert_eq!(RetryPolicy::load_from(&path), policy);
    }
}
