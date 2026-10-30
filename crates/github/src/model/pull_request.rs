use super::{
    Actor, Check, ChecksState, Label, MergeMethod, Mergeability, PullRequestState, ReviewDecision,
    ReviewVerdict, UpdateMethod,
};
use crate::PullRequestRef;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The account a credential signs in as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Viewer {
    pub id: u64,
    pub login: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListState {
    All,
    #[default]
    Open,
    /// Closed without being merged.
    Closed,
    Merged,
}

/// Whose pull requests a listing shows, relative to the viewer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Involvement {
    #[default]
    All,
    Authored,
    ReviewRequested,
}

/// One page of a repository's pull requests.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ListQuery {
    pub state: ListState,
    pub involvement: Involvement,
    /// Free text, searched as one phrase.
    pub text: Option<String>,
    /// At most 100.
    pub limit: u32,
    /// `ListPage::next_cursor` of the previous page.
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListPage {
    pub entries: Vec<ListEntry>,
    /// Opaque; set while more pages follow.
    pub next_cursor: Option<String>,
}

/// A pull request as a listing row shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListEntry {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: Option<Actor>,
    pub head_branch: String,
    pub base_branch: String,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub mergeability: Mergeability,
    pub review_decision: Option<ReviewDecision>,
    pub additions: u64,
    pub deletions: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Users asked for a review who have not answered yet.
    pub review_request_logins: Vec<String>,
    /// A team was asked for a review.
    pub has_team_review_request: bool,
    pub labels: Vec<Label>,
    pub checks_state: Option<ChecksState>,
}

/// Line counts of one pull request, as `Client::list_stats` reads them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffStat {
    pub pr: PullRequestRef,
    pub additions: u64,
    pub deletions: u64,
}

/// The small, batched read of a pull request (lifecycle sweeps, badges).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub head_branch: String,
    pub base_branch: String,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub closed_at: Option<DateTime<Utc>>,
    pub merged_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub author: Option<Actor>,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub review_decision: Option<ReviewDecision>,
    pub checks_state: Option<ChecksState>,
    pub mergeability: Mergeability,
}

/// A pull request found by its head branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadPullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub head_branch: String,
    pub base_branch: String,
    pub head_sha: Option<String>,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub updated_at: DateTime<Utc>,
    pub is_cross_repository: bool,
    /// `owner/name` of the repository the head branch lives in.
    pub head_repository: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeCapabilities {
    pub merge: bool,
    pub squash: bool,
    pub rebase: bool,
}

/// Whether the head branch has fallen behind the base branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BaseComparison {
    UpToDate,
    Behind,
    Unknown,
}

/// The kinds of action `Client::run_action` performs, as permissions name them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PullRequestAction {
    Merge,
    Ready,
    Draft,
    Close,
    Reopen,
    DeleteBranch,
    UpdateBranch,
    EnableAutoMerge,
    DisableAutoMerge,
}

/// An action on a pull request with what it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "action", content = "method")]
pub enum Action {
    /// Merges now; where the base has a merge queue, through it.
    Merge(MergeMethod),
    /// Arms auto-merge, or merges at once when nothing is pending.
    EnableAutoMerge(MergeMethod),
    DisableAutoMerge,
    /// Brings the base branch into the head; nothing when not behind.
    UpdateBranch(UpdateMethod),
    Ready,
    Draft,
    Close,
    Reopen,
    /// Deletes the source branch of a closed or merged pull request.
    DeleteBranch,
}

impl Action {
    pub fn kind(self) -> PullRequestAction {
        match self {
            Action::Merge(_) => PullRequestAction::Merge,
            Action::EnableAutoMerge(_) => PullRequestAction::EnableAutoMerge,
            Action::DisableAutoMerge => PullRequestAction::DisableAutoMerge,
            Action::UpdateBranch(_) => PullRequestAction::UpdateBranch,
            Action::Ready => PullRequestAction::Ready,
            Action::Draft => PullRequestAction::Draft,
            Action::Close => PullRequestAction::Close,
            Action::Reopen => PullRequestAction::Reopen,
            Action::DeleteBranch => PullRequestAction::DeleteBranch,
        }
    }
}

/// What the viewer may do with a pull request. GitHub refuses whatever
/// it did not report, so unknown standing is granted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewerPermissions {
    pub actions: Vec<PullRequestAction>,
    pub comment: bool,
    pub resolve: bool,
    /// Authors may only comment on their own pull request.
    pub verdicts: Vec<ReviewVerdict>,
    pub request_reviewers: bool,
    pub update_methods: Vec<UpdateMethod>,
    pub labels: bool,
    /// May switch "Allow edits by maintainers".
    pub maintainer_edits: bool,
}

/// The full read of one pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detail {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub url: String,
    pub author: Option<Actor>,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub mergeability: Mergeability,
    pub review_decision: Option<ReviewDecision>,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub head_branch: String,
    pub head_sha: Option<String>,
    /// `owner/name` of the repository the head branch lives in.
    pub head_repository: Option<String>,
    pub head_repository_owner: Option<String>,
    pub is_cross_repository: bool,
    pub base_branch: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub merged_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    /// Outstanding review requests: users, then teams (`org/slug`).
    pub reviewers: Vec<Actor>,
    pub labels: Vec<Label>,
    pub checks: Vec<Check>,
    pub checks_state: Option<ChecksState>,
    /// More checks exist than were read (over 1000).
    pub checks_truncated: bool,
    /// Merge methods the repository allows.
    pub merge_capabilities: MergeCapabilities,
    pub auto_merge_allowed: bool,
    pub viewer_permissions: ViewerPermissions,
    pub base_comparison: BaseComparison,
    pub behind_by: Option<u64>,
    pub auto_merge_enabled: bool,
    pub auto_merge_method: Option<MergeMethod>,
    pub maintainer_can_modify: bool,
}

/// Fields of a pull request to rewrite; `None` leaves one as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestUpdate {
    pub title: Option<String>,
    pub body: Option<String>,
    pub maintainer_can_modify: Option<bool>,
}
