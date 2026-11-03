//! The quota GitHub reports in `x-ratelimit-*` headers, per host and
//! resource (`core` for REST, `graphql`). Nothing is debited locally: a 304
//! and a request that never reached GitHub spend nothing, so local counts
//! would drift. Only what GitHub last said counts.

use crate::transport::Headers;
use std::collections::HashMap;

/// Background requests keep this share of the quota for interactive ones.
const RESERVE_RATIO: f64 = 0.1;

#[derive(Debug, Clone, Copy)]
struct Snapshot {
    limit: u64,
    remaining: u64,
    reset_at: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Quota {
    snapshots: HashMap<(String, String), Snapshot>,
}

impl Quota {
    /// `Err(reset_at)` when the request would dip below its floor: 10 %
    /// remaining for background requests, 1 for interactive ones.
    pub fn admit(
        &self,
        host: &str,
        resource: &str,
        allow_reserve: bool,
        now: u64,
    ) -> Result<(), u64> {
        let Some(snapshot) = self
            .snapshots
            .get(&(host.to_string(), resource.to_string()))
        else {
            return Ok(());
        };
        if snapshot.reset_at <= now {
            return Ok(());
        }
        let floor = if allow_reserve {
            1.0
        } else {
            snapshot.limit as f64 * RESERVE_RATIO
        };
        if snapshot.remaining as f64 >= floor {
            Ok(())
        } else {
            Err(snapshot.reset_at)
        }
    }

    /// Records the balance an answer reported. Within one window the latest
    /// answer wins (regional answers may raise it again); an answer from an
    /// older window is ignored.
    pub fn observe(&mut self, host: &str, headers: &Headers) {
        let number = |name: &str| headers.get(name).and_then(|v| v.trim().parse::<u64>().ok());
        let Some(resource) = headers
            .get("x-ratelimit-resource")
            .map(str::trim)
            .filter(|r| !r.is_empty())
        else {
            return;
        };
        let (Some(limit), Some(remaining), Some(reset)) = (
            number("x-ratelimit-limit"),
            number("x-ratelimit-remaining"),
            number("x-ratelimit-reset"),
        ) else {
            return;
        };
        if limit == 0 {
            return;
        }
        let key = (host.to_string(), resource.to_string());
        let reset_at = reset * 1000;
        if let Some(previous) = self.snapshots.get(&key)
            && reset_at < previous.reset_at
        {
            return;
        }
        self.snapshots.insert(
            key,
            Snapshot {
                limit,
                remaining,
                reset_at,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000;

    fn headers(resource: &str, limit: u64, remaining: u64, reset_s: u64) -> Headers {
        Headers::new()
            .with("x-ratelimit-resource", resource)
            .with("x-ratelimit-limit", limit.to_string())
            .with("x-ratelimit-remaining", remaining.to_string())
            .with("x-ratelimit-reset", reset_s.to_string())
    }

    #[test]
    fn refuses_a_background_request_below_the_reserve_per_host_and_resource() {
        let mut quota = Quota::default();
        quota.observe("github.com", &headers("graphql", 5000, 499, 2000));
        assert_eq!(
            quota.admit("github.com", "graphql", false, NOW),
            Err(2_000_000)
        );
        assert!(quota.admit("github.com", "core", false, NOW).is_ok());
        assert!(quota.admit("ghe.example", "graphql", false, NOW).is_ok());
        assert!(quota.admit("github.com", "graphql", true, NOW).is_ok());
    }

    #[test]
    fn lets_interactive_requests_spend_the_reserve_but_not_an_empty_quota() {
        let mut quota = Quota::default();
        quota.observe("github.com", &headers("graphql", 5000, 0, 2000));
        assert!(quota.admit("github.com", "graphql", true, NOW).is_err());
        // Past the reset the old balance no longer applies.
        assert!(
            quota
                .admit("github.com", "graphql", false, 2_000_000)
                .is_ok()
        );
    }

    #[test]
    fn takes_the_latest_balance_in_a_window_and_ignores_an_older_window() {
        let mut quota = Quota::default();
        quota.observe("github.com", &headers("graphql", 5000, 100, 2000));
        assert!(quota.admit("github.com", "graphql", false, NOW).is_err());
        quota.observe("github.com", &headers("graphql", 5000, 4000, 2000));
        assert!(quota.admit("github.com", "graphql", false, NOW).is_ok());
        quota.observe("github.com", &headers("graphql", 5000, 10, 1500));
        assert!(quota.admit("github.com", "graphql", false, NOW).is_ok());
    }

    #[test]
    fn ignores_answers_without_complete_quota_headers() {
        let mut quota = Quota::default();
        quota.observe(
            "github.com",
            &Headers::new().with("x-ratelimit-remaining", "0"),
        );
        assert!(quota.admit("github.com", "core", false, NOW).is_ok());
    }
}
