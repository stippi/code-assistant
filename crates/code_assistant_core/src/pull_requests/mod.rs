//! GitHub pull requests for the views: which repository and pull request a
//! session belongs to, and a read cache in front of the `github` client.
//!
//! Every read is served from memory when it can be: a value younger than
//! its part's time to live as it is, an older one at once while a
//! background refresh runs. When the refresh lands, an app-scoped
//! [`UiEvent::PullRequestChanged`] (or [`UiEvent::PullRequestListChanged`])
//! tells the views to read again. Identical reads in flight share one
//! request. Writes invalidate the parts they change and announce them the
//! same way. The last details, conversations and lists are kept on disk,
//! so a cold start shows them at once and revalidates.
//!
//! [`UiEvent::PullRequestChanged`]: crate::ui::UiEvent::PullRequestChanged
//! [`UiEvent::PullRequestListChanged`]: crate::ui::UiEvent::PullRequestListChanged

mod cache;
mod checkout;
mod repo;
mod snapshots;
mod store;
#[cfg(test)]
mod tests;

pub use checkout::{CheckoutPlace, PullRequestCheckout};
pub use store::{PullRequestSource, PullRequests};

pub(crate) use checkout::pull_request_head;

pub(crate) use repo::{github_remote, github_repo, own_pull_request};

use github::RepoRef;
use std::time::SystemTime;

/// A cached read: the value plus whether a background refresh is under way
/// (a `PullRequestChanged` follows when it lands).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched<T> {
    pub value: T,
    /// When GitHub answered with this value.
    pub fetched_at: SystemTime,
    pub refreshing: bool,
}

/// The separately cached parts of a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PullRequestPart {
    Detail,
    Checks,
    Activity,
    Diff,
    FilesViewed,
}

/// Where a session's pull requests live and which one is its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPullRequests {
    pub repo: RepoRef,
    /// The branch the session works on, if any.
    pub head_branch: Option<String>,
    /// The number of the pull request whose head is that branch.
    pub own: Option<u64>,
}
