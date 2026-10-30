//! Polling checks cheaply. Before the full read, the pull request, its
//! head's check runs and its head's statuses are revalidated with ETags; a
//! 304 costs no quota. When none of them changed and the last read is
//! recent, that read is the answer. Hosts without these endpoints fall back
//! to the full read.

use super::pr_variables;
use crate::PullRequestRef;
use crate::client::{Client, RestCall};
use crate::error::{Error, Result};
use crate::model::Checks;
use lru::LruCache;
use serde::Deserialize;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;

const CAPACITY: usize = 128;
/// A read nothing has invalidated still counts for this long.
const FRESH_FOR: Duration = Duration::from_secs(5 * 60);
/// Pages of one endpoint at most.
const MAX_ENDPOINT_PAGES: u32 = 100;

#[derive(Default)]
struct Entry {
    head_sha: Option<String>,
    value: Option<(Checks, Instant)>,
    /// The host lacks one of the endpoints; only full reads from now on.
    unsupported: bool,
}

/// Per pull request: the last head, the last read, and one poll at a time.
pub(crate) struct ChecksRevalidator {
    entries: Mutex<LruCache<String, Arc<tokio::sync::Mutex<Entry>>>>,
}

impl ChecksRevalidator {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(LruCache::new(NonZeroUsize::new(CAPACITY).unwrap())),
        }
    }

    fn entry(&self, key: String) -> Arc<tokio::sync::Mutex<Entry>> {
        self.entries
            .lock()
            .unwrap()
            .get_or_insert(key, Default::default)
            .clone()
    }
}

/// What a revalidation learned.
enum Probe {
    /// The endpoint answered; whether its answer changed.
    Answered { changed: bool, has_next: bool },
    /// The host lacks the endpoint, or it failed in a way the full read may not.
    Unavailable { unsupported: bool },
}

fn has_next_page(link: Option<&str>) -> bool {
    link.is_some_and(|link| link.contains("rel=\"next\""))
}

impl Client {
    async fn probe(&self, host: &str, path: String) -> Result<(Probe, Vec<u8>)> {
        match self
            .rest_revalidated(RestCall::get(host, "revalidateChecks", path))
            .await
        {
            Ok(answer) => Ok((
                Probe::Answered {
                    changed: answer.changed,
                    has_next: has_next_page(answer.headers.get("link")),
                },
                answer.body,
            )),
            Err(Error::NotFound { .. }) => {
                Ok((Probe::Unavailable { unsupported: true }, Vec::new()))
            }
            Err(Error::Api { status, .. }) => Ok((
                Probe::Unavailable {
                    unsupported: matches!(status, 404 | 405 | 501),
                },
                Vec::new(),
            )),
            Err(e) => Err(e),
        }
    }

    /// Revalidates every page of one endpoint: whether any changed.
    async fn probe_pages(&self, host: &str, endpoint: &str) -> Result<Result<bool, bool>> {
        let mut changed = false;
        for page in 1..=MAX_ENDPOINT_PAGES {
            let separator = if endpoint.contains('?') { '&' } else { '?' };
            let path = format!("{endpoint}{separator}per_page=100&page={page}");
            match self.probe(host, path).await?.0 {
                Probe::Unavailable { unsupported } => return Ok(Err(unsupported)),
                Probe::Answered {
                    changed: page_changed,
                    has_next,
                } => {
                    changed |= page_changed;
                    if !has_next {
                        return Ok(Ok(changed));
                    }
                }
            }
        }
        Ok(Ok(true))
    }

    /// The pull request's state and its head's checks, read in full only
    /// when something changed or the last read is older than five minutes.
    pub async fn checks(&self, pr: &PullRequestRef) -> Result<Checks> {
        pr_variables(pr)?;
        let entry = self.inner.checks.entry(format!("{pr}"));
        let mut entry = entry.lock().await;
        if entry.unsupported {
            return self.read_checks(pr).await;
        }
        let host = &pr.repo.host;
        let root = format!("repos/{}/{}", pr.repo.owner, pr.repo.name);

        let (probe, body) = self
            .probe(host, format!("{root}/pulls/{}", pr.number))
            .await?;
        let mut changed = match probe {
            Probe::Answered { changed, .. } => changed,
            Probe::Unavailable { unsupported } => {
                entry.unsupported = unsupported;
                entry.value = None;
                return self.read_checks(pr).await;
            }
        };
        #[derive(Deserialize)]
        struct Head {
            sha: String,
        }
        #[derive(Deserialize)]
        struct Pull {
            head: Head,
        }
        let head = serde_json::from_slice::<Pull>(&body)
            .map_err(|e| Error::decode("revalidateChecks", e))?
            .head
            .sha;
        if entry.head_sha.as_deref() != Some(head.as_str()) {
            changed = true;
            entry.head_sha = Some(head.clone());
        }

        let runs = format!("{root}/commits/{head}/check-runs?filter=all");
        let statuses = format!("{root}/commits/{head}/status");
        let (runs, statuses) = futures::try_join!(
            self.probe_pages(host, &runs),
            self.probe_pages(host, &statuses)
        )?;
        match (runs, statuses) {
            (Ok(runs_changed), Ok(statuses_changed)) => {
                changed |= runs_changed || statuses_changed;
            }
            (Err(unsupported), _) | (_, Err(unsupported)) => {
                entry.unsupported = unsupported;
                entry.value = None;
                return self.read_checks(pr).await;
            }
        }
        if changed {
            entry.value = None;
        }
        if let Some((value, read_at)) = &entry.value
            && read_at.elapsed() < FRESH_FOR
        {
            return Ok(value.clone());
        }
        let value = self.read_checks(pr).await?;
        // Kept only when it read the head the probes saw.
        entry.value = (value.head_sha.as_deref() == Some(head.as_str()))
            .then(|| (value.clone(), Instant::now()));
        Ok(value)
    }

    async fn read_checks(&self, pr: &PullRequestRef) -> Result<Checks> {
        let detail = self.detail(pr).await?;
        Ok(Checks {
            state: detail.state,
            head_sha: detail.head_sha,
            checks: detail.checks,
            checks_state: detail.checks_state,
        })
    }
}
