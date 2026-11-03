//! Pausing a host after GitHub refused a request for quota reasons.
//!
//! Each `check` hands out a lease (the pause generation it saw). A refusal
//! recorded against an older lease than the current generation only extends
//! the pause, so a slow request that started before a pause cannot restart
//! the backoff; a success clears the backoff only for the generation it saw.

use std::collections::HashMap;

const FALLBACK_COOLDOWN_MS: u64 = 30_000;
const MAX_FALLBACK_COOLDOWN_MS: u64 = 15 * 60_000;

#[derive(Debug, Clone, Copy, Default)]
struct Entry {
    attempt: u32,
    generation: u64,
    retry_at: u64,
}

#[derive(Debug, Default)]
pub(crate) struct HostPauses {
    entries: HashMap<String, Entry>,
}

fn fallback_cooldown(attempt: u32) -> u64 {
    let doublings = attempt.saturating_sub(1).min(16);
    (FALLBACK_COOLDOWN_MS << doublings).min(MAX_FALLBACK_COOLDOWN_MS)
}

impl HostPauses {
    /// The lease for a request, or `Err(retry_at)` while the host is paused.
    /// Interactive requests pass a pause (`allow_paused`) without lifting it.
    pub fn check(&self, host: &str, now: u64, allow_paused: bool) -> Result<u64, u64> {
        let entry = self.entries.get(host).copied().unwrap_or_default();
        if entry.retry_at > now && !allow_paused {
            Err(entry.retry_at)
        } else {
            Ok(entry.generation)
        }
    }

    /// Records a refusal. Without a `retry_at` from GitHub the pause is 30 s,
    /// doubling with each refusal in a row up to 15 minutes.
    pub fn record_rate_limit(&mut self, host: &str, lease: u64, retry_at: Option<u64>, now: u64) {
        let previous = self.entries.get(host).copied();
        if let Some(previous) = previous
            && previous.generation > lease
        {
            // A newer refusal is already recorded; this one may only extend it.
            if previous.retry_at <= now && retry_at.is_none_or(|at| at <= now) {
                return;
            }
            if let Some(at) = retry_at
                && at > previous.retry_at
            {
                self.entries.insert(
                    host.to_string(),
                    Entry {
                        retry_at: at,
                        ..previous
                    },
                );
            }
            return;
        }
        let attempt = previous.map_or(0, |p| p.attempt) + 1;
        let proposed = match retry_at {
            Some(at) if at > now => at,
            _ => now + fallback_cooldown(attempt),
        };
        let retry_at = match previous {
            Some(previous) if previous.retry_at > now => previous.retry_at.max(proposed),
            _ => proposed,
        };
        self.entries.insert(
            host.to_string(),
            Entry {
                attempt,
                generation: previous.map_or(0, |p| p.generation).max(lease) + 1,
                retry_at,
            },
        );
    }

    /// Records an answer that was not refused: the backoff starts over, as
    /// long as no refusal came in since this request's lease.
    pub fn record_success(&mut self, host: &str, lease: u64, now: u64) {
        if let Some(entry) = self.entries.get_mut(host)
            && entry.generation == lease
            && entry.retry_at <= now
        {
            entry.attempt = 0;
            entry.retry_at = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "github.com";

    #[test]
    fn pauses_a_host_until_github_says_and_keeps_others_open() {
        let mut pauses = HostPauses::default();
        let lease = pauses.check(HOST, 0, false).unwrap();
        pauses.record_rate_limit(HOST, lease, Some(60_000), 0);
        assert_eq!(pauses.check(HOST, 1_000, false), Err(60_000));
        assert!(pauses.check("ghe.example", 1_000, false).is_ok());
        assert!(pauses.check(HOST, 60_000, false).is_ok());
    }

    #[test]
    fn backs_off_from_30_seconds_doubling_up_to_15_minutes() {
        let mut pauses = HostPauses::default();
        let mut now = 0;
        let mut pauses_seen = Vec::new();
        for _ in 0..8 {
            let lease = pauses.check(HOST, now, true).unwrap();
            pauses.record_rate_limit(HOST, lease, None, now);
            let retry_at = pauses.check(HOST, now, false).unwrap_err();
            pauses_seen.push(retry_at - now);
            now = retry_at;
        }
        assert_eq!(
            pauses_seen,
            [
                30_000, 60_000, 120_000, 240_000, 480_000, 900_000, 900_000, 900_000
            ]
        );
    }

    #[test]
    fn lets_an_interactive_request_through_without_lifting_the_pause() {
        let mut pauses = HostPauses::default();
        let lease = pauses.check(HOST, 0, false).unwrap();
        pauses.record_rate_limit(HOST, lease, Some(121_000), 0);
        let interactive = pauses.check(HOST, 1_000, true).unwrap();
        pauses.record_success(HOST, interactive, 1_000);
        assert_eq!(pauses.check(HOST, 2_000, false), Err(121_000));
    }

    #[test]
    fn an_older_success_does_not_clear_a_concurrent_pause() {
        let mut pauses = HostPauses::default();
        let first = pauses.check(HOST, 0, false).unwrap();
        let second = pauses.check(HOST, 0, false).unwrap();
        pauses.record_rate_limit(HOST, first, None, 0);
        pauses.record_success(HOST, second, 0);
        assert_eq!(pauses.check(HOST, 1_000, false), Err(30_000));
    }

    #[test]
    fn a_stale_refusal_extends_but_does_not_restart_the_backoff() {
        let mut pauses = HostPauses::default();
        let stale = pauses.check(HOST, 0, false).unwrap();
        pauses.record_rate_limit(HOST, stale, None, 0);
        // The same old request reports again, with a later reset.
        pauses.record_rate_limit(HOST, stale, Some(61_000), 0);
        assert_eq!(pauses.check(HOST, 0, false), Err(61_000));
        // After the pause a fresh refusal is the second in a row: 60 s.
        let lease = pauses.check(HOST, 61_000, false).unwrap();
        pauses.record_rate_limit(HOST, lease, None, 61_000);
        assert_eq!(pauses.check(HOST, 61_000, false), Err(121_000));
    }
}
