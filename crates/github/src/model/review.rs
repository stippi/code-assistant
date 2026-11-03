use super::DiffSide;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewVerdict {
    Comment,
    Approve,
    RequestChanges,
}

impl ReviewVerdict {
    pub const ALL: [ReviewVerdict; 3] = [
        ReviewVerdict::Comment,
        ReviewVerdict::Approve,
        ReviewVerdict::RequestChanges,
    ];
}

/// A review sent in one piece: verdict, summary and line comments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSubmission {
    pub verdict: ReviewVerdict,
    pub body: String,
    pub comments: Vec<ReviewCommentDraft>,
}

/// A line comment of a review. A range runs from `start_line` on
/// `start_side` to `line` on `side`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewCommentDraft {
    pub path: String,
    pub line: u64,
    pub side: DiffSide,
    pub start_line: Option<u64>,
    pub start_side: Option<DiffSide>,
    pub body: String,
}

/// Which kind of comment an edit rewrites; GitHub has a mutation for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EditableCommentKind {
    IssueComment,
    ReviewComment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewerKind {
    User,
    Team,
}

/// Someone a review can be asked of: a user by login, a team by slug.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Reviewer {
    pub id: String,
    pub kind: ReviewerKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewerCandidate {
    /// Login for a user, slug for a team.
    pub id: String,
    pub kind: ReviewerKind,
    /// `org/slug` for a team, matching `Detail::reviewers`.
    pub login: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    pub is_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewerCandidates {
    pub candidates: Vec<ReviewerCandidate>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelCandidate {
    pub name: String,
    pub color: Option<String>,
    pub description: Option<String>,
    pub is_applied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelCandidates {
    /// Applied labels the repository no longer defines come first.
    pub candidates: Vec<LabelCandidate>,
    pub truncated: bool,
}
