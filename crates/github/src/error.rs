//! What can go wrong talking to GitHub.

use serde::{Deserialize, Serialize};
use std::time::SystemTime;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// One entry of a GraphQL answer's `errors` list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphQlError {
    #[serde(default)]
    pub message: String,
    /// GitHub's error class, e.g. `NOT_FOUND`, `FORBIDDEN`, `RATE_LIMITED`.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// Where in the answer the error sits: field names and list indexes.
    #[serde(default)]
    pub path: Vec<PathSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PathSegment {
    Field(String),
    Index(u64),
}

impl GraphQlError {
    pub(crate) fn is_kind(&self, kind: &str) -> bool {
        self.kind.as_deref() == Some(kind)
    }
}

/// Why deleting a pull request's source branch was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchDeletionRefusal {
    OpenPullRequest,
    SourceUnavailable,
    UnsafeBranch,
    HeadChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileUnavailableReason {
    Oversized,
    Binary,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    #[error("No GitHub credential for {host}: {detail}")]
    NoCredential { host: String, detail: String },

    #[error(
        "GitHub refused the credential for {host}. Run `gh auth login --hostname {host}` and retry."
    )]
    Unauthorized { host: String },

    #[error("GitHub API rate limit exceeded on {host}.")]
    RateLimited {
        host: String,
        retry_at: Option<SystemTime>,
    },

    /// Refused locally: the quota is spent or an earlier refusal paused the host.
    #[error("GitHub requests to {host} are paused until the rate limit resets.")]
    Paused { host: String, retry_at: SystemTime },

    #[error("GitHub could not find what {operation} asked for, or the credential cannot see it.")]
    NotFound { operation: &'static str },

    #[error("GitHub refused {operation} (HTTP {status}){}", joined(messages))]
    Api {
        operation: &'static str,
        status: u16,
        messages: Vec<String>,
    },

    #[error("GitHub refused {operation}{}", joined_graphql(errors))]
    GraphQl {
        operation: &'static str,
        errors: Vec<GraphQlError>,
    },

    #[error("Could not reach GitHub at {host} ({operation}): {detail}")]
    Request {
        host: String,
        operation: &'static str,
        detail: String,
    },

    #[error("GitHub returned an unreadable {operation} response: {detail}")]
    Decode {
        operation: &'static str,
        detail: String,
    },

    /// The caller named something GitHub cannot address: a host, repository,
    /// cursor or commit that is not well-formed.
    #[error("{0}")]
    Invalid(String),

    #[error("The named subject does not belong to this pull request.")]
    SubjectScope,

    #[error("{}", branch_deletion_message(*.0))]
    BranchDeletionRefused(BranchDeletionRefusal),

    #[error("The file '{path}' is {}.", match reason {
        FileUnavailableReason::Oversized => "larger than 1 MB",
        FileUnavailableReason::Binary => "binary",
    })]
    FileUnavailable {
        path: String,
        reason: FileUnavailableReason,
    },

    #[error("GitHub reported no usable base and head revisions.")]
    RevisionsUnavailable,
}

impl Error {
    /// GitHub refused for quota reasons, or the client did not ask at all.
    pub fn is_rate_limited(&self) -> bool {
        matches!(self, Error::RateLimited { .. } | Error::Paused { .. })
    }

    pub fn is_not_found(&self) -> bool {
        matches!(self, Error::NotFound { .. })
    }

    pub(crate) fn decode(operation: &'static str, detail: impl ToString) -> Self {
        Error::Decode {
            operation,
            detail: detail.to_string(),
        }
    }
}

fn joined(messages: &[String]) -> String {
    if messages.is_empty() {
        String::new()
    } else {
        format!(": {}", messages.join("; "))
    }
}

fn joined_graphql(errors: &[GraphQlError]) -> String {
    let messages: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
    joined(&messages)
}

fn branch_deletion_message(reason: BranchDeletionRefusal) -> &'static str {
    match reason {
        BranchDeletionRefusal::OpenPullRequest => {
            "Close or merge the pull request before deleting its source branch."
        }
        BranchDeletionRefusal::SourceUnavailable => {
            "GitHub did not report a complete source branch for the pull request."
        }
        BranchDeletionRefusal::UnsafeBranch => {
            "The source branch is a default or base branch and cannot be deleted here."
        }
        BranchDeletionRefusal::HeadChanged => {
            "The source branch has new commits and was not deleted."
        }
    }
}
