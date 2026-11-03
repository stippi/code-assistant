use super::{Actor, DiffSide, Reaction};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The conversation of a pull request: remarks, review threads, commits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    pub author: Option<Actor>,
    /// Requested reviewers and everyone who reviewed, each once.
    pub reviewers: Vec<Actor>,
    /// Issue comments, reviews and review-thread comments, oldest first.
    pub comments: Vec<Comment>,
    pub comment_count: u64,
    /// Some comments were not read (long threads, many threads).
    pub comments_truncated: bool,
    pub review_threads: Vec<ReviewThread>,
    pub review_threads_truncated: bool,
    /// Up to the newest hundred, oldest first.
    pub commits: Vec<Commit>,
    /// Reactions on the pull request itself.
    pub reactions: Vec<Reaction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommentKind {
    /// A remark on the conversation.
    IssueComment,
    /// A remark in a review thread on a line.
    ReviewComment,
    /// A review's summary and verdict.
    Review,
}

/// The verdict of a review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewState {
    Commented,
    Approved,
    ChangesRequested,
    Dismissed,
    Pending,
}

impl ReviewState {
    pub(crate) fn from_graphql(state: &str) -> Option<Self> {
        match state.trim().to_ascii_uppercase().as_str() {
            "COMMENTED" => Some(ReviewState::Commented),
            "APPROVED" => Some(ReviewState::Approved),
            "CHANGES_REQUESTED" => Some(ReviewState::ChangesRequested),
            "DISMISSED" => Some(ReviewState::Dismissed),
            "PENDING" => Some(ReviewState::Pending),
            _ => None,
        }
    }

    /// A review that says something by its state alone.
    pub(crate) fn is_verdict(self) -> bool {
        matches!(
            self,
            ReviewState::Approved | ReviewState::ChangesRequested | ReviewState::Dismissed
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    /// GraphQL node id: what edits and reactions address.
    pub id: String,
    pub kind: CommentKind,
    pub author: Option<Actor>,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
    pub url: Option<String>,
    /// The file of a review comment.
    pub path: Option<String>,
    pub review_state: Option<ReviewState>,
    pub reactions: Vec<Reaction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewThread {
    pub id: String,
    pub path: String,
    /// `None` once the line has left the diff (outdated).
    pub line: Option<u64>,
    pub side: DiffSide,
    pub is_resolved: bool,
    pub is_outdated: bool,
    /// The first comments; `next_comments_cursor` reads the rest.
    pub comments: Vec<ThreadComment>,
    /// All comments, read or not.
    pub comment_count: u64,
    pub next_comments_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadComment {
    pub id: String,
    pub author: Option<Actor>,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
    pub url: Option<String>,
    pub reactions: Vec<Reaction>,
}

/// More comments of one review thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadCommentsPage {
    pub comments: Vec<ThreadComment>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub oid: String,
    pub message_headline: String,
    pub committed_date: DateTime<Utc>,
    /// Unknown for merge commits, whose stats would include the base's changes.
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub authors: Vec<Actor>,
}
