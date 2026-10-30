//! `PullRequestCore` and `PullRequestCheckContexts` → `Detail`.

use super::checks::{self, RawCheck};
use super::list::RawCommitNode;
use super::{
    Connection, RawActor, RawLabel, actor, labels, login_avatar_url, merge_method, mergeability,
    review_decision, state, trimmed,
};
use crate::model::{
    Actor, BaseComparison, Detail, MergeCapabilities, PullRequestAction, ReviewVerdict,
    UpdateMethod, ViewerPermissions,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;

/// Merge states in which `gh pr merge --auto` merges at once.
pub(crate) const IMMEDIATELY_MERGEABLE: [&str; 3] = ["CLEAN", "HAS_HOOKS", "UNSTABLE"];

/// Write access. GitHub answers a null permission only to a GitHub App,
/// whose rights no API states, so null is granted and GitHub refuses
/// whatever the app was not given.
pub(crate) fn can_write(permission: Option<&str>) -> bool {
    match permission {
        None => true,
        Some(p) => matches!(
            p.trim().to_ascii_uppercase().as_str(),
            "ADMIN" | "MAINTAIN" | "WRITE"
        ),
    }
}

fn can_triage(permission: Option<&str>) -> bool {
    permission.is_some_and(|p| p.trim().eq_ignore_ascii_case("TRIAGE")) || can_write(permission)
}

/// What the viewer may do, as far as GitHub said.
#[derive(Debug, Clone, Default)]
pub(crate) struct ViewerAccess {
    pub can_write: bool,
    pub can_delete_branch: bool,
    pub can_triage: bool,
    pub can_update: bool,
    pub did_author: bool,
    pub can_update_branch: bool,
    pub can_merge_now: bool,
    pub auto_merge_allowed: bool,
    pub can_edit_maintainer_edits: bool,
}

/// Asking someone else for a review needs write access, which an author
/// of a pull request from a fork does not have.
pub(crate) fn viewer_permissions(access: &ViewerAccess) -> ViewerPermissions {
    let mut actions = Vec::new();
    if access.can_delete_branch {
        actions.push(PullRequestAction::DeleteBranch);
    }
    if access.can_write {
        actions.push(PullRequestAction::Merge);
        actions.push(PullRequestAction::ApproveWorkflows);
        if access.auto_merge_allowed && !access.can_merge_now {
            actions.push(PullRequestAction::EnableAutoMerge);
        }
        // Disabling stays possible after the repository turned auto-merge off.
        actions.push(PullRequestAction::DisableAutoMerge);
    }
    if access.can_update {
        actions.extend([
            PullRequestAction::Ready,
            PullRequestAction::Draft,
            PullRequestAction::Close,
            PullRequestAction::Reopen,
        ]);
    }
    if access.can_update_branch {
        actions.push(PullRequestAction::UpdateBranch);
    }
    ViewerPermissions {
        actions,
        comment: true,
        resolve: access.can_write || access.did_author,
        // GitHub refuses an author's approval or change request.
        verdicts: if access.did_author {
            vec![ReviewVerdict::Comment]
        } else {
            ReviewVerdict::ALL.to_vec()
        },
        request_reviewers: access.can_write,
        update_methods: if access.can_update_branch {
            vec![UpdateMethod::Merge, UpdateMethod::Rebase]
        } else {
            Vec::new()
        },
        labels: access.can_triage,
        maintainer_edits: access.can_edit_maintainer_edits,
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct CoreData {
    #[serde(default)]
    repository: Option<RawCoreRepository>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCoreRepository {
    #[serde(default)]
    merge_commit_allowed: bool,
    #[serde(default)]
    squash_merge_allowed: bool,
    #[serde(default)]
    rebase_merge_allowed: bool,
    #[serde(default)]
    auto_merge_allowed: bool,
    #[serde(default)]
    viewer_permission: Option<String>,
    #[serde(default)]
    pull_request: Option<RawCorePullRequest>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawHeadRepository {
    name_with_owner: Option<String>,
    viewer_permission: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawAutoMergeRequest {
    merge_method: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawCompare {
    behind_by: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawBaseRef {
    compare: Option<RawCompare>,
}

/// A requested reviewer: a user or bot (login), or a team (slug).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RawReviewer {
    #[serde(rename = "__typename")]
    pub typename: Option<String>,
    pub login: Option<String>,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    pub slug: Option<String>,
    pub combined_slug: Option<String>,
    pub team_name: Option<String>,
    pub team_avatar_url: Option<String>,
}

impl RawReviewer {
    pub fn as_actor(&self) -> Option<Actor> {
        actor(Some(&RawActor {
            typename: self.typename.clone(),
            login: self.login.clone(),
            name: self.name.clone(),
            avatar_url: self.avatar_url.clone(),
        }))
    }

    /// A team goes by `org/slug`, apart from any user sharing its slug.
    pub fn as_team(&self) -> Option<Actor> {
        Some(Actor {
            login: trimmed(self.combined_slug.as_deref())?,
            name: trimmed(self.team_name.as_deref()),
            avatar_url: trimmed(self.team_avatar_url.as_deref()),
            is_bot: false,
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RawReviewRequest {
    pub requested_reviewer: Option<RawReviewer>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawContexts {
    contexts: Connection<RawCheck>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawContextsCommit {
    status_check_rollup: Option<RawContexts>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawId {
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCorePullRequest {
    number: u64,
    title: String,
    url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(default)]
    merge_state_status: Option<String>,
    #[serde(default)]
    review_decision: Option<String>,
    #[serde(default)]
    additions: u64,
    #[serde(default)]
    deletions: u64,
    #[serde(default)]
    changed_files: u64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    merged_at: Option<DateTime<Utc>>,
    #[serde(default)]
    closed_at: Option<DateTime<Utc>>,
    head_ref_name: String,
    base_ref_name: String,
    #[serde(default)]
    head_ref_oid: Option<String>,
    #[serde(default)]
    is_cross_repository: bool,
    #[serde(default)]
    maintainer_can_modify: bool,
    #[serde(default)]
    head_repository_owner: Option<RawActor>,
    #[serde(default)]
    head_repository: Option<RawHeadRepository>,
    #[serde(default)]
    head_ref: Option<RawId>,
    #[serde(default)]
    author: Option<RawActor>,
    #[serde(default)]
    auto_merge_request: Option<RawAutoMergeRequest>,
    #[serde(default)]
    viewer_can_update: Option<bool>,
    #[serde(default)]
    viewer_did_author: Option<bool>,
    #[serde(default)]
    viewer_can_update_branch: bool,
    #[serde(default)]
    base_ref: Option<RawBaseRef>,
    #[serde(default)]
    review_requests: Connection<RawReviewRequest>,
    #[serde(default)]
    labels: Connection<RawLabel>,
    #[serde(default)]
    commits: Connection<RawCommitNode<RawContextsCommit>>,
}

/// An author without an avatar gets the one GitHub serves for the login.
pub(crate) fn with_fallback_avatar(actor: Option<Actor>, host: &str) -> Option<Actor> {
    actor.map(|mut actor| {
        if actor.avatar_url.is_none() {
            actor.avatar_url = login_avatar_url(&actor.login, host);
        }
        actor
    })
}

/// The detail, and whether more check contexts exist than its first page.
pub(crate) fn core(data: CoreData, host: &str) -> Option<(Detail, bool)> {
    let repository = data.repository?;
    let pr = repository.pull_request?;
    let state = state(pr.state.as_deref(), pr.merged_at.is_some());
    let open = pr
        .state
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("OPEN"));
    let contexts = pr
        .commits
        .nodes()
        .next()
        .and_then(|node| node.commit.status_check_rollup.as_ref())
        .map(|rollup| &rollup.contexts);
    let raw_checks: Vec<RawCheck> = contexts
        .map(|c| c.nodes().cloned().collect())
        .unwrap_or_default();
    let more_checks = contexts.is_some_and(|c| c.has_next_page());
    let behind_by = pr
        .base_ref
        .as_ref()
        .and_then(|base| base.compare.as_ref())
        .filter(|_| open)
        .map(|compare| compare.behind_by);
    let did_author = pr.viewer_did_author == Some(true);
    let access = ViewerAccess {
        can_write: can_write(repository.viewer_permission.as_deref()),
        can_delete_branch: pr
            .head_repository
            .as_ref()
            .is_some_and(|r| can_write(r.viewer_permission.as_deref()))
            && pr.head_ref.is_some(),
        can_triage: can_triage(repository.viewer_permission.as_deref()),
        can_update: pr.viewer_can_update != Some(false),
        did_author,
        can_update_branch: behind_by.is_some() && pr.viewer_can_update_branch,
        can_merge_now: pr
            .merge_state_status
            .as_deref()
            .is_some_and(|s| IMMEDIATELY_MERGEABLE.contains(&s)),
        auto_merge_allowed: repository.auto_merge_allowed,
        can_edit_maintainer_edits: open
            && pr.is_cross_repository
            && did_author
            && pr
                .head_repository_owner
                .as_ref()
                .is_some_and(|owner| owner.typename.as_deref() == Some("User"))
            && pr.head_repository.is_some()
            && pr.head_ref.is_some(),
    };
    let requested: Vec<&RawReviewer> = pr
        .review_requests
        .nodes()
        .filter_map(|r| r.requested_reviewer.as_ref())
        .collect();
    let reviewers = requested
        .iter()
        .filter_map(|r| with_fallback_avatar(r.as_actor(), host))
        .chain(requested.iter().filter_map(|r| r.as_team()))
        .collect();
    let detail = Detail {
        number: pr.number,
        title: pr.title,
        body: pr.body.unwrap_or_default(),
        url: pr.url,
        author: with_fallback_avatar(actor(pr.author.as_ref()), host),
        state,
        is_draft: pr.is_draft,
        mergeability: mergeability(pr.mergeable.as_deref()),
        review_decision: review_decision(pr.review_decision.as_deref()),
        additions: pr.additions,
        deletions: pr.deletions,
        changed_files: pr.changed_files,
        head_branch: pr.head_ref_name,
        head_sha: trimmed(pr.head_ref_oid.as_deref()),
        head_repository: pr
            .head_repository
            .as_ref()
            .and_then(|r| trimmed(r.name_with_owner.as_deref())),
        head_repository_owner: pr
            .head_repository_owner
            .as_ref()
            .and_then(|o| trimmed(o.login.as_deref())),
        is_cross_repository: pr.is_cross_repository,
        base_branch: pr.base_ref_name,
        created_at: pr.created_at,
        updated_at: pr.updated_at,
        merged_at: pr.merged_at,
        closed_at: pr.closed_at,
        reviewers,
        labels: labels(&pr.labels),
        checks: checks::checks(&raw_checks),
        checks_state: checks::checks_state(&raw_checks),
        checks_truncated: false,
        merge_capabilities: MergeCapabilities {
            merge: repository.merge_commit_allowed,
            squash: repository.squash_merge_allowed,
            rebase: repository.rebase_merge_allowed,
        },
        auto_merge_allowed: repository.auto_merge_allowed,
        viewer_permissions: viewer_permissions(&access),
        base_comparison: match behind_by {
            None | Some(None) => BaseComparison::Unknown,
            Some(Some(0)) => BaseComparison::UpToDate,
            Some(Some(_)) => BaseComparison::Behind,
        },
        behind_by: behind_by.flatten(),
        auto_merge_enabled: pr.auto_merge_request.is_some(),
        auto_merge_method: pr
            .auto_merge_request
            .as_ref()
            .and_then(|r| merge_method(r.merge_method.as_deref())),
        maintainer_can_modify: pr.maintainer_can_modify,
        workflow_approvals: None,
    };
    Some((detail, more_checks))
}

#[derive(Debug, Deserialize)]
pub(crate) struct ContextsData {
    #[serde(default)]
    repository: Option<RawContextsRepository>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawContextsRepository {
    pull_request: Option<RawContextsPullRequest>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawContextsPullRequest {
    head_ref_oid: Option<String>,
    commits: Connection<RawCommitNode<RawContextsCommit>>,
}

/// One page of a head commit's check contexts.
pub(crate) struct ContextsPage {
    pub head_sha: Option<String>,
    pub contexts: Vec<RawCheck>,
    pub next_cursor: Option<String>,
}

pub(crate) fn contexts_page(data: ContextsData) -> Option<ContextsPage> {
    let pr = data.repository?.pull_request?;
    let contexts = pr
        .commits
        .nodes()
        .next()
        .and_then(|node| node.commit.status_check_rollup.as_ref())
        .map(|rollup| &rollup.contexts);
    Some(ContextsPage {
        head_sha: trimmed(pr.head_ref_oid.as_deref()),
        contexts: contexts
            .map(|c| c.nodes().cloned().collect())
            .unwrap_or_default(),
        next_cursor: contexts.and_then(Connection::next_cursor),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CheckStatus, ChecksState, MergeMethod, Mergeability, PullRequestState, ReviewDecision,
    };
    use serde_json::Value;

    fn fixture() -> Value {
        serde_json::from_str(include_str!("../../tests/fixtures/pull_request_core.json")).unwrap()
    }

    fn read(data: Value) -> (Detail, bool) {
        core(serde_json::from_value(data).unwrap(), "github.com").unwrap()
    }

    #[test]
    fn reads_the_detail_of_an_open_pull_request() {
        let (detail, more_checks) = read(fixture()["data"].clone());
        assert!(!more_checks);
        assert_eq!(detail.number, 291);
        assert_eq!(detail.state, PullRequestState::Open);
        assert_eq!(detail.mergeability, Mergeability::Mergeable);
        assert_eq!(detail.review_decision, Some(ReviewDecision::ReviewRequired));
        assert_eq!(
            detail.head_sha.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert_eq!(
            detail.head_repository.as_deref(),
            Some("contributor/code-assistant")
        );
        assert!(detail.is_cross_repository);
        assert_eq!(detail.body, "Adds the pull request view.");
        assert_eq!(detail.labels.len(), 2);
        assert_eq!(detail.behind_by, Some(3));
        assert_eq!(detail.base_comparison, BaseComparison::Behind);
        assert!(detail.auto_merge_enabled);
        assert_eq!(detail.auto_merge_method, Some(MergeMethod::Squash));
        assert_eq!(
            detail.merge_capabilities,
            MergeCapabilities {
                merge: false,
                squash: true,
                rebase: true
            }
        );
    }

    #[test]
    fn lists_requested_users_then_teams_by_org_slug() {
        let (detail, _) = read(fixture()["data"].clone());
        let logins: Vec<_> = detail.reviewers.iter().map(|r| r.login.as_str()).collect();
        assert_eq!(logins, ["octocat", "review-bot", "acme/core"]);
        assert_eq!(detail.reviewers[0].name.as_deref(), Some("The Octocat"));
        assert!(detail.reviewers[1].is_bot);
        assert_eq!(detail.reviewers[2].name.as_deref(), Some("Core"));
        // Requested people show their avatar, from GitHub or by login.
        assert!(detail.reviewers[0].avatar_url.is_some());
        assert!(detail.reviewers[1].avatar_url.is_some());
    }

    #[test]
    fn folds_a_rerun_check_into_its_newest_run_failures_first() {
        let (detail, _) = read(fixture()["data"].clone());
        let checks: Vec<_> = detail
            .checks
            .iter()
            .map(|c| (c.name.as_str(), c.status))
            .collect();
        assert_eq!(
            checks,
            [
                ("lint", CheckStatus::Failure),
                ("test", CheckStatus::Success),
                ("ci/legacy", CheckStatus::Pending),
            ]
        );
        assert_eq!(detail.checks[1].required, Some(true));
        assert_eq!(detail.checks_state, Some(ChecksState::Failing));
    }

    #[test]
    fn grants_a_writer_everything_but_author_only_verdicts_to_the_author() {
        let (detail, _) = read(fixture()["data"].clone());
        let permissions = &detail.viewer_permissions;
        assert_eq!(
            permissions.actions,
            [
                PullRequestAction::DeleteBranch,
                PullRequestAction::Merge,
                PullRequestAction::ApproveWorkflows,
                PullRequestAction::EnableAutoMerge,
                PullRequestAction::DisableAutoMerge,
                PullRequestAction::Ready,
                PullRequestAction::Draft,
                PullRequestAction::Close,
                PullRequestAction::Reopen,
                PullRequestAction::UpdateBranch,
            ]
        );
        assert_eq!(permissions.verdicts, [ReviewVerdict::Comment]);
        assert!(permissions.resolve);
        assert!(permissions.request_reviewers);
        assert!(permissions.labels);
        assert_eq!(
            permissions.update_methods,
            [UpdateMethod::Merge, UpdateMethod::Rebase]
        );
        assert!(permissions.maintainer_edits);
    }

    #[test]
    fn says_no_to_a_reader_who_can_only_read() {
        let mut data = fixture()["data"].clone();
        data["repository"]["viewerPermission"] = "READ".into();
        data["repository"]["pullRequest"]["viewerDidAuthor"] = false.into();
        data["repository"]["pullRequest"]["viewerCanUpdate"] = false.into();
        data["repository"]["pullRequest"]["viewerCanUpdateBranch"] = false.into();
        data["repository"]["pullRequest"]["headRepository"]["viewerPermission"] = "READ".into();
        let (detail, _) = read(data);
        let permissions = &detail.viewer_permissions;
        assert!(permissions.actions.is_empty());
        assert!(!permissions.resolve);
        assert!(!permissions.request_reviewers);
        assert!(!permissions.labels);
        assert_eq!(permissions.verdicts, ReviewVerdict::ALL);
        assert!(!permissions.maintainer_edits);
    }

    #[test]
    fn grants_write_to_a_github_app_answered_with_a_null_permission() {
        assert!(can_write(None));
        assert!(can_write(Some("maintain")));
        assert!(!can_write(Some("TRIAGE")));
        assert!(can_triage(Some("TRIAGE")));
        assert!(!can_triage(Some("READ")));
    }

    #[test]
    fn offers_no_auto_merge_where_the_merge_would_happen_at_once() {
        let mut data = fixture()["data"].clone();
        data["repository"]["pullRequest"]["mergeStateStatus"] = "CLEAN".into();
        let (detail, _) = read(data);
        assert!(
            !detail
                .viewer_permissions
                .actions
                .contains(&PullRequestAction::EnableAutoMerge)
        );
    }

    #[test]
    fn a_closed_pull_request_has_no_base_comparison() {
        let mut data = fixture()["data"].clone();
        data["repository"]["pullRequest"]["state"] = "CLOSED".into();
        data["repository"]["pullRequest"]["closedAt"] = "2026-10-30T10:00:00Z".into();
        data["repository"]["pullRequest"]["autoMergeRequest"] = Value::Null;
        let (detail, _) = read(data);
        assert_eq!(detail.state, PullRequestState::Closed);
        assert_eq!(detail.base_comparison, BaseComparison::Unknown);
        assert_eq!(detail.behind_by, None);
        assert!(!detail.auto_merge_enabled);
        assert!(!detail.viewer_permissions.maintainer_edits);
    }

    #[test]
    fn reports_more_check_contexts_past_the_first_page() {
        let mut data = fixture()["data"].clone();
        data["repository"]["pullRequest"]["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]
            ["pageInfo"]["hasNextPage"] = true.into();
        assert!(read(data).1);
    }
}
