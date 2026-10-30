//! The client: one per process, shared by every caller. It holds the
//! token cache, the request slots, quota and pause state, the ETag cache
//! and the node-id cache, so all reads of a host share one view of them.

mod cache;
mod http;

pub(crate) use http::RestCall;

use crate::auth::{GhTokenSource, TokenCache, TokenSource};
use crate::limits::{HostPauses, Quota};
use crate::ops::checks::ChecksRevalidator;
use crate::transport::{ReqwestTransport, Transport};
use cache::{EtagCache, NodeIdCache};
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

/// At most this many requests in flight: GitHub's secondary limits punish
/// concurrent bursts.
const CONCURRENT_REQUESTS: usize = 8;

/// Who is waiting for a request. Background requests leave the last 10 %
/// of a quota to interactive ones and stop while a host is paused;
/// interactive requests may spend the quota down to its last request and
/// pass a pause.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Priority {
    #[default]
    Background,
    Interactive,
}

#[derive(Default)]
pub(crate) struct Limits {
    pub quota: Quota,
    pub pauses: HostPauses,
}

pub(crate) struct Inner {
    pub transport: Arc<dyn Transport>,
    pub tokens: TokenCache,
    pub slots: Semaphore,
    pub limits: Mutex<Limits>,
    pub etags: EtagCache,
    pub node_ids: NodeIdCache,
    pub checks: ChecksRevalidator,
}

/// A GitHub API client for pull request reads and writes. Cheap to clone;
/// clones share all state.
#[derive(Clone)]
pub struct Client {
    pub(crate) inner: Arc<Inner>,
    pub(crate) priority: Priority,
}

impl Client {
    /// Talks to GitHub over HTTPS with the token from the environment or `gh`.
    pub fn new() -> Self {
        Self::with_parts(Arc::new(ReqwestTransport::new()), Arc::new(GhTokenSource))
    }

    /// A client over the given transport and token source (tests use a
    /// scripted transport).
    pub fn with_parts(transport: Arc<dyn Transport>, tokens: Arc<dyn TokenSource>) -> Self {
        Self {
            inner: Arc::new(Inner {
                transport,
                tokens: TokenCache::new(tokens),
                slots: Semaphore::new(CONCURRENT_REQUESTS),
                limits: Mutex::new(Limits::default()),
                etags: EtagCache::new(),
                node_ids: NodeIdCache::new(),
                checks: ChecksRevalidator::new(),
            }),
            priority: Priority::Background,
        }
    }

    /// A handle sharing this client's state whose requests run at `priority`.
    pub fn with_priority(&self, priority: Priority) -> Self {
        Self {
            inner: self.inner.clone(),
            priority,
        }
    }

    pub fn priority(&self) -> Priority {
        self.priority
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}
