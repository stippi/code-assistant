//! One read cache per kind of value: each entry holds the last value read,
//! when it goes stale, and the read under way, if any.
//!
//! - A value younger than its time to live is served as it is.
//! - Up to [`LAST_GOOD`] past that, it is served at once while a background
//!   read refreshes it; the cache's `refreshed` callback runs when that read
//!   lands, so views can read again.
//! - With nothing servable (no value, too old, or invalidated) the reader
//!   waits for the read. If it fails for a passing reason (rate limit,
//!   network, server error), a value within that window is served instead.
//! - Concurrent readers of one key share one read. Invalidation drops the
//!   read under way, so a result that started before a write never replaces
//!   what the next read brings.

use super::Fetched;
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use github::Priority;
use lru::LruCache;
use std::hash::Hash;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::time::Instant;
use tracing::{debug, warn};

/// How long past its time to live a value is still served: while it is
/// refreshed, or when it cannot be.
pub(super) const LAST_GOOD: Duration = Duration::from_secs(10 * 60);

/// Distinguishes reads across all caches, so a read never mistakes another
/// one for itself.
static NEXT_READ: AtomicU64 = AtomicU64::new(1);

type Read<V> = Shared<BoxFuture<'static, github::Result<(V, SystemTime)>>>;

/// Runs when a background refresh of a key landed, stored or failed.
pub(super) type Refreshed<K> = Box<dyn Fn(&K) + Send + Sync>;

/// What a value weighs in bytes.
pub(super) type Weight<V> = fn(&V) -> usize;

/// How a cache treats its values.
pub(super) struct Policy<K, V> {
    pub ttl: fn(&K, &V) -> Duration,
    pub capacity: usize,
    /// What a value weighs in bytes, and how much all values may weigh
    /// together. A value heavier than that is not kept.
    pub budget: Option<(Weight<V>, usize)>,
}

struct Held<V> {
    value: V,
    fetched_at: SystemTime,
    expires: Instant,
    keep_until: Instant,
}

impl<V: Clone> Held<V> {
    fn fetched(&self, refreshing: bool) -> Fetched<V> {
        Fetched {
            value: self.value.clone(),
            fetched_at: self.fetched_at,
            refreshing,
        }
    }
}

struct InFlight<V> {
    id: u64,
    read: Read<V>,
    background: bool,
}

struct Entry<V> {
    held: Option<Held<V>>,
    in_flight: Option<InFlight<V>>,
    /// A write changed what the value shows: readers wait for a new read.
    invalidated: bool,
}

impl<V> Default for Entry<V> {
    fn default() -> Self {
        Self {
            held: None,
            in_flight: None,
            invalidated: false,
        }
    }
}

struct Inner<K, V> {
    policy: Policy<K, V>,
    entries: Mutex<LruCache<K, Entry<V>>>,
    refreshed: Option<Refreshed<K>>,
}

pub(super) struct Cache<K, V> {
    inner: Arc<Inner<K, V>>,
}

impl<K, V> Cache<K, V>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    pub fn new(policy: Policy<K, V>, refreshed: Option<Refreshed<K>>) -> Self {
        let capacity = NonZeroUsize::new(policy.capacity).expect("a cache holds something");
        Self {
            inner: Arc::new(Inner {
                policy,
                entries: Mutex::new(LruCache::new(capacity)),
                refreshed,
            }),
        }
    }

    /// The value for `key`, read with `fetch` when nothing servable is held.
    /// `fetch` runs on its own task at the priority it is given: interactive
    /// when a reader waits for it, background for a refresh.
    pub async fn read<F>(&self, key: K, fetch: F) -> github::Result<Fetched<V>>
    where
        F: FnOnce(Priority) -> BoxFuture<'static, github::Result<V>>,
    {
        let now = Instant::now();
        let read = {
            let mut entries = self.inner.entries.lock().unwrap();
            let entry = entries.get_or_insert_mut(key.clone(), Entry::default);
            if !entry.invalidated
                && let Some(held) = &entry.held
            {
                if now < held.expires {
                    return Ok(held.fetched(false));
                }
                if now < held.keep_until {
                    let fetched = held.fetched(true);
                    if entry.in_flight.is_none() {
                        entry.in_flight = Some(self.start(key, true, fetch));
                    }
                    return Ok(fetched);
                }
            }
            entry
                .in_flight
                .get_or_insert_with(|| self.start(key.clone(), false, fetch))
                .read
                .clone()
        };
        match read.await {
            Ok((value, fetched_at)) => Ok(Fetched {
                value,
                fetched_at,
                refreshing: false,
            }),
            Err(error) if keeps_last_good(&error) => match self.last_good(&key) {
                Some(held) => {
                    warn!("Serving an earlier pull request read after a failed one: {error}");
                    Ok(held)
                }
                None => Err(error),
            },
            Err(error) => Err(error),
        }
    }

    fn start<F>(&self, key: K, background: bool, fetch: F) -> InFlight<V>
    where
        F: FnOnce(Priority) -> BoxFuture<'static, github::Result<V>>,
    {
        let id = NEXT_READ.fetch_add(1, Ordering::Relaxed);
        let priority = if background {
            Priority::Background
        } else {
            Priority::Interactive
        };
        let fetching = fetch(priority);
        let inner = self.inner.clone();
        let read = async move {
            let result = fetching.await;
            let fetched_at = SystemTime::now();
            inner.land(&key, id, &result, fetched_at);
            result.map(|value| (value, fetched_at))
        }
        .boxed()
        .shared();
        // Finishes and lands even when every reader has gone.
        tokio::spawn(read.clone());
        InFlight {
            id,
            read,
            background,
        }
    }

    fn last_good(&self, key: &K) -> Option<Fetched<V>> {
        let entries = self.inner.entries.lock().unwrap();
        let held = entries.peek(key)?.held.as_ref()?;
        (Instant::now() < held.keep_until).then(|| held.fetched(false))
    }

    /// Whether a value is held for `key`, servable or not.
    pub fn holds(&self, key: &K) -> bool {
        let entries = self.inner.entries.lock().unwrap();
        entries.peek(key).is_some_and(|entry| entry.held.is_some())
    }

    /// Looks at the value held for `key` without reading.
    pub fn peek<R>(&self, key: &K, look: impl FnOnce(&V) -> R) -> Option<R> {
        let entries = self.inner.entries.lock().unwrap();
        entries
            .peek(key)
            .and_then(|entry| entry.held.as_ref())
            .map(|held| look(&held.value))
    }

    /// Holds a value read earlier (a disk snapshot) as already stale: it is
    /// served once, at once, while a refresh runs. Ignored when the key
    /// holds a value by now.
    pub fn seed(&self, key: K, value: V, fetched_at: SystemTime) {
        let now = Instant::now();
        let mut entries = self.inner.entries.lock().unwrap();
        let entry = entries.get_or_insert_mut(key, Entry::default);
        if entry.held.is_none() {
            entry.held = Some(Held {
                value,
                fetched_at,
                expires: now,
                keep_until: now + LAST_GOOD,
            });
        }
    }

    /// The next read of every matching key waits for a new value; reads
    /// under way are dropped.
    pub fn invalidate(&self, matches: impl Fn(&K) -> bool) {
        let mut entries = self.inner.entries.lock().unwrap();
        for (key, entry) in entries.iter_mut() {
            if matches(key) {
                entry.invalidated = true;
                entry.in_flight = None;
            }
        }
    }
}

impl<K, V> Inner<K, V>
where
    K: Hash + Eq + Clone,
    V: Clone,
{
    /// Stores what a read brought, unless the read was dropped meanwhile.
    fn land(&self, key: &K, id: u64, result: &github::Result<V>, fetched_at: SystemTime) {
        let background = {
            let mut entries = self.entries.lock().unwrap();
            let Some(entry) = entries.peek_mut(key) else {
                return;
            };
            let Some(read) = entry.in_flight.take_if(|read| read.id == id) else {
                return;
            };
            let now = Instant::now();
            match result {
                Ok(value) if self.fits(value) => {
                    let expires = now + (self.policy.ttl)(key, value);
                    entry.held = Some(Held {
                        value: value.clone(),
                        fetched_at,
                        expires,
                        keep_until: expires + LAST_GOOD,
                    });
                    entry.invalidated = false;
                    self.keep_within_budget(&mut entries);
                }
                Ok(_) => entry.held = None,
                Err(error) => {
                    debug!("A pull request read failed: {error}");
                    // A failed refresh waits a time to live before the next
                    // one, so views reading again do not retry in a loop.
                    if read.background
                        && let Some(held) = &mut entry.held
                    {
                        held.expires = now + (self.policy.ttl)(key, &held.value);
                    }
                }
            }
            read.background
        };
        if background && let Some(refreshed) = &self.refreshed {
            refreshed(key);
        }
    }

    fn fits(&self, value: &V) -> bool {
        self.policy
            .budget
            .is_none_or(|(weight, budget)| weight(value) <= budget)
    }

    fn keep_within_budget(&self, entries: &mut LruCache<K, Entry<V>>) {
        let Some((weight, budget)) = self.policy.budget else {
            return;
        };
        loop {
            let total: usize = entries
                .iter()
                .filter_map(|(_, entry)| entry.held.as_ref())
                .map(|held| weight(&held.value))
                .sum();
            if total <= budget || entries.pop_lru().is_none() {
                return;
            }
        }
    }
}

/// Failures that say nothing about the pull request: the value read
/// before is still the best answer.
fn keeps_last_good(error: &github::Error) -> bool {
    match error {
        github::Error::Api { status, .. } => *status >= 500,
        error => error.is_rate_limited() || matches!(error, github::Error::Request { .. }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    const TTL: Duration = Duration::from_secs(60);

    fn cache(refreshed: Option<Refreshed<u32>>) -> Cache<u32, String> {
        Cache::new(
            Policy {
                ttl: |_, _| TTL,
                capacity: 8,
                budget: Some((String::len, 10)),
            },
            refreshed,
        )
    }

    /// A fetch that answers `value` and counts its calls.
    fn answer(
        calls: &Arc<AtomicUsize>,
        value: &str,
    ) -> impl FnOnce(Priority) -> BoxFuture<'static, github::Result<String>> + use<> {
        let calls = calls.clone();
        let value = value.to_string();
        move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { Ok(value) }.boxed()
        }
    }

    fn failure() -> github::Error {
        github::Error::Request {
            host: "github.com".into(),
            operation: "test",
            detail: "offline".into(),
        }
    }

    fn fail(_: Priority) -> BoxFuture<'static, github::Result<String>> {
        async { Err(failure()) }.boxed()
    }

    /// Lets spawned reads run to their end.
    async fn settle() {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_fresh_value_is_served_without_reading() {
        let cache = cache(None);
        let calls = Arc::new(AtomicUsize::new(0));
        let first = cache.read(1, answer(&calls, "a")).await.unwrap();
        tokio::time::advance(TTL - Duration::from_secs(1)).await;
        let second = cache.read(1, answer(&calls, "b")).await.unwrap();
        assert_eq!(first.value, "a");
        assert_eq!(second, first);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_reads_share_one_fetch() {
        let cache = cache(None);
        let calls = Arc::new(AtomicUsize::new(0));
        let (a, b) = tokio::join!(
            cache.read(1, answer(&calls, "a")),
            cache.read(1, answer(&calls, "b"))
        );
        assert_eq!(a.unwrap().value, "a");
        assert_eq!(b.unwrap().value, "a");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stale_value_is_served_while_a_background_read_refreshes_it() {
        let refreshed = Arc::new(AtomicUsize::new(0));
        let counter = refreshed.clone();
        let cache = cache(Some(Box::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })));
        let calls = Arc::new(AtomicUsize::new(0));
        cache.read(1, answer(&calls, "a")).await.unwrap();
        tokio::time::advance(TTL).await;

        let priority = Arc::new(Mutex::new(None));
        let seen = priority.clone();
        let stale = cache
            .read(1, move |p| {
                *seen.lock().unwrap() = Some(p);
                async { Ok("b".to_string()) }.boxed()
            })
            .await
            .unwrap();
        assert_eq!(stale.value, "a");
        assert!(stale.refreshing);
        settle().await;

        assert_eq!(*priority.lock().unwrap(), Some(Priority::Background));
        assert_eq!(refreshed.load(Ordering::SeqCst), 1);
        let fresh = cache.read(1, answer(&calls, "c")).await.unwrap();
        assert_eq!(fresh.value, "b");
        assert!(!fresh.refreshing);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_refresh_keeps_the_value_and_waits_a_time_to_live() {
        let cache = cache(Some(Box::new(|_| {})));
        let calls = Arc::new(AtomicUsize::new(0));
        cache.read(1, answer(&calls, "a")).await.unwrap();
        tokio::time::advance(TTL).await;

        assert!(cache.read(1, fail).await.unwrap().refreshing);
        settle().await;
        let after = cache.read(1, answer(&calls, "b")).await.unwrap();

        assert_eq!(after.value, "a");
        assert!(!after.refreshing);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_value_past_the_last_good_window_is_waited_for() {
        let cache = cache(None);
        let calls = Arc::new(AtomicUsize::new(0));
        cache.read(1, answer(&calls, "a")).await.unwrap();
        tokio::time::advance(TTL + LAST_GOOD).await;

        let read = cache.read(1, answer(&calls, "b")).await.unwrap();
        assert_eq!(read.value, "b");
        assert!(!read.refreshing);
        // Too old to stand in for a failed read either.
        tokio::time::advance(TTL + LAST_GOOD).await;
        assert!(cache.read(1, fail).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn an_invalidated_value_is_waited_for_and_stands_in_when_the_read_fails() {
        let cache = cache(None);
        let calls = Arc::new(AtomicUsize::new(0));
        cache.read(1, answer(&calls, "a")).await.unwrap();
        cache.invalidate(|key| *key == 1);

        let read = cache.read(1, answer(&calls, "b")).await.unwrap();
        assert_eq!(read.value, "b");

        cache.invalidate(|key| *key == 1);
        assert_eq!(cache.read(1, fail).await.unwrap().value, "b");
        let refused = github::Error::NotFound { operation: "test" };
        cache.invalidate(|key| *key == 1);
        let error = cache
            .read(1, move |_| async move { Err(refused) }.boxed())
            .await
            .unwrap_err();
        assert!(error.is_not_found());
    }

    #[tokio::test(start_paused = true)]
    async fn a_read_started_before_an_invalidation_is_not_kept() {
        let cache = cache(None);
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let early = cache.read(1, move |_| {
            async move {
                released.await.unwrap();
                Ok("old".to_string())
            }
            .boxed()
        });
        let write = async {
            settle().await;
            cache.invalidate(|key| *key == 1);
            release.send(()).unwrap();
        };
        let (early, ()) = tokio::join!(early, write);
        // The reader that waited still gets its answer...
        assert_eq!(early.unwrap().value, "old");
        // ...but the next one reads again.
        let calls = Arc::new(AtomicUsize::new(0));
        let read = cache.read(1, answer(&calls, "new")).await.unwrap();
        assert_eq!(read.value, "new");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn values_stay_within_the_byte_budget() {
        let cache = cache(None);
        let calls = Arc::new(AtomicUsize::new(0));
        cache.read(1, answer(&calls, "aaaa")).await.unwrap();
        cache.read(2, answer(&calls, "bbbb")).await.unwrap();
        cache.read(3, answer(&calls, "cccc")).await.unwrap();
        cache
            .read(4, answer(&calls, "far too heavy"))
            .await
            .unwrap();

        assert!(!cache.holds(&1), "the oldest value goes first");
        assert!(cache.holds(&2) && cache.holds(&3));
        assert!(!cache.holds(&4), "a value over the budget is not kept");
    }
}
