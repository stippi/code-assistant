//! Model values for the pull request view's tests: a plain open pull
//! request each, changed by the test where it matters.

use chrono::{DateTime, Duration, TimeZone, Utc};
use github::RepoRef;
use github::model::{
    Actor, BaseComparison, Check, CheckStatus, Detail, ListEntry, MergeCapabilities, Mergeability,
    PullRequestAction, PullRequestState, ViewerPermissions,
};

/// The fixed "now" relative times are measured from.
pub fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 30, 12, 0, 0).unwrap()
}

pub fn hours_ago(hours: i64) -> DateTime<Utc> {
    now() - Duration::hours(hours)
}

pub fn repo() -> RepoRef {
    RepoRef::new("github.com", "stippi", "code-assistant")
}

pub fn actor(login: &str) -> Actor {
    Actor {
        login: login.into(),
        name: None,
        avatar_url: None,
        is_bot: false,
    }
}

/// An open, mergeable pull request by `author`, updated `number` hours ago.
pub fn entry(number: u64, title: &str, author: &str) -> ListEntry {
    ListEntry {
        number,
        title: title.into(),
        url: format!("https://github.com/stippi/code-assistant/pull/{number}"),
        author: Some(actor(author)),
        head_branch: format!("feature/{number}"),
        base_branch: "main".into(),
        state: PullRequestState::Open,
        is_draft: false,
        mergeability: Mergeability::Mergeable,
        review_decision: None,
        additions: 0,
        deletions: 0,
        created_at: hours_ago(100 + number as i64),
        updated_at: hours_ago(number as i64),
        review_request_logins: Vec::new(),
        has_team_review_request: false,
        labels: Vec::new(),
        checks_state: None,
        stack: None,
    }
}

/// `entry` changed by `change`.
pub fn entry_with(
    number: u64,
    title: &str,
    author: &str,
    change: impl FnOnce(&mut ListEntry),
) -> ListEntry {
    let mut entry = entry(number, title, author);
    change(&mut entry);
    entry
}

pub fn check(name: &str, status: CheckStatus) -> Check {
    Check {
        name: name.into(),
        status,
        description: None,
        url: None,
        required: None,
    }
}

/// An open, mergeable pull request the viewer may do everything with.
pub fn detail(number: u64) -> Detail {
    Detail {
        number,
        title: "feat(files): show raster images in the Files view".into(),
        body: "## What\n\nImages show as images.".into(),
        url: format!("https://github.com/stippi/code-assistant/pull/{number}"),
        author: Some(actor("daniel-kurzynski")),
        state: PullRequestState::Open,
        is_draft: false,
        mergeability: Mergeability::Mergeable,
        review_decision: None,
        additions: 432,
        deletions: 46,
        changed_files: 6,
        head_branch: "feat/image-file-renderer".into(),
        head_sha: Some("d9aea40".into()),
        head_repository: Some("stippi/code-assistant".into()),
        head_repository_owner: Some("stippi".into()),
        is_cross_repository: false,
        base_branch: "main".into(),
        created_at: hours_ago(18),
        updated_at: hours_ago(9),
        merged_at: None,
        closed_at: None,
        reviewers: Vec::new(),
        labels: Vec::new(),
        checks: Vec::new(),
        checks_state: None,
        checks_truncated: false,
        merge_capabilities: MergeCapabilities {
            merge: true,
            squash: true,
            rebase: true,
        },
        auto_merge_allowed: true,
        viewer_permissions: ViewerPermissions {
            actions: vec![
                PullRequestAction::Merge,
                PullRequestAction::Ready,
                PullRequestAction::Draft,
                PullRequestAction::Close,
                PullRequestAction::Reopen,
                PullRequestAction::DeleteBranch,
                PullRequestAction::UpdateBranch,
                PullRequestAction::EnableAutoMerge,
                PullRequestAction::DisableAutoMerge,
            ],
            comment: true,
            resolve: true,
            verdicts: Vec::new(),
            request_reviewers: true,
            update_methods: Vec::new(),
            labels: true,
            maintainer_edits: false,
        },
        base_comparison: BaseComparison::UpToDate,
        behind_by: None,
        auto_merge_enabled: false,
        auto_merge_method: None,
        maintainer_can_modify: false,
        workflow_approvals: None,
    }
}

/// `detail` changed by `change`.
pub fn detail_with(number: u64, change: impl FnOnce(&mut Detail)) -> Detail {
    let mut detail = detail(number);
    change(&mut detail);
    detail
}
