//! Which tools the model is offered.
//!
//! An application may withdraw tools at any time (see [`ToolAvailability`]),
//! but the tool list is the start of the prefix prompt caches key on (tools →
//! system prompt → messages). Changing it mid-run, or while the provider still
//! holds the cache of the previous run, would re-bill the whole conversation.
//! So the offer follows the application lazily: it is settled only when a run
//! starts and the last request is old enough for the cache to have expired.
//! Until then a withdrawn tool stays in the request, and a call of it is
//! answered with [`ToolDisabled`] — for every tool, without the tool knowing.

use std::any::Any;
use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};

/// How long a provider keeps a prompt cache after the last request that used
/// it. Anthropic's default cache lives five minutes; providers that keep it
/// longer only lose a cache that may still have been warm.
pub const DEFAULT_PROMPT_CACHE_TTL: Duration = Duration::from_secs(5 * 60);

/// Tells the loop which tools the application has withdrawn.
pub trait ToolAvailability: Send + Sync {
    /// Names of the tools withdrawn right now. Asked when a run starts (to
    /// settle the offer) and before every dispatch (to refuse calls of tools
    /// withdrawn since). `extensions` is the agent's loop state (see
    /// [`crate::hooks::LoopCtx::extensions`]); the answer may change at any
    /// time, e.g. through a handle the application shares with its UI.
    fn withdrawn(&self, extensions: &(dyn Any + Send)) -> BTreeSet<String>;

    /// How long a prompt cache outlives the request that used it.
    fn cache_ttl(&self) -> Duration {
        DEFAULT_PROMPT_CACHE_TTL
    }
}

/// Withdraws nothing.
pub struct AllToolsAvailable;

impl ToolAvailability for AllToolsAvailable {
    fn withdrawn(&self, _: &(dyn Any + Send)) -> BTreeSet<String> {
        BTreeSet::new()
    }
}

/// The tools to hold back from the next run's requests: the application's
/// wish once the cache of the previous request has expired (or there is
/// none), otherwise what the previous requests held back.
pub fn settle_withheld(
    previous: &BTreeSet<String>,
    withdrawn: BTreeSet<String>,
    last_request: Option<SystemTime>,
    now: SystemTime,
    cache_ttl: Duration,
) -> BTreeSet<String> {
    let cache_warm = last_request.is_some_and(|at| {
        now.duration_since(at)
            .map(|age| age < cache_ttl)
            // A clock that went backwards: the request counts as recent.
            .unwrap_or(true)
    });
    if cache_warm {
        previous.clone()
    } else {
        withdrawn
    }
}

/// The answer to a call of a withdrawn tool.
#[derive(Debug)]
pub struct ToolDisabled(pub String);

impl std::fmt::Display for ToolDisabled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "The tool '{}' has been disabled. Do not use it anymore.",
            self.0
        )
    }
}

impl std::error::Error for ToolDisabled {}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    const TTL: Duration = Duration::from_secs(300);

    #[test]
    fn a_warm_cache_keeps_the_previous_offer() {
        let now = SystemTime::now();
        let settled = settle_withheld(
            &set(&[]),
            set(&["name_session"]),
            Some(now - Duration::from_secs(60)),
            now,
            TTL,
        );
        assert!(settled.is_empty());

        // Tools come back lazily too.
        let settled = settle_withheld(
            &set(&["name_session"]),
            set(&[]),
            Some(now - Duration::from_secs(60)),
            now,
            TTL,
        );
        assert_eq!(settled, set(&["name_session"]));
    }

    #[test]
    fn an_expired_or_missing_cache_takes_the_applications_wish() {
        let now = SystemTime::now();
        for last_request in [None, Some(now - TTL), Some(now - 2 * TTL)] {
            let settled = settle_withheld(
                &set(&["old"]),
                set(&["name_session"]),
                last_request,
                now,
                TTL,
            );
            assert_eq!(settled, set(&["name_session"]), "{last_request:?}");
        }
    }

    #[test]
    fn a_request_from_the_future_counts_as_recent() {
        let now = SystemTime::now();
        let settled = settle_withheld(
            &set(&[]),
            set(&["name_session"]),
            Some(now + Duration::from_secs(60)),
            now,
            TTL,
        );
        assert!(settled.is_empty());
    }
}
