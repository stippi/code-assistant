//! Listing rows, batched summaries and head-branch lookups.

use super::checks::{RawCheck, checks_state};
use super::{
    Connection, RawActor, RawLabel, RawLatestReview, actor, labels, mergeability,
    review_decision_with_reviews, state, trimmed,
};
use crate::model::{HeadPullRequest, ListEntry, Summary};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RawRequestedReviewer {
    pub login: Option<String>,
    pub slug: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RawReviewRequest {
    pub requested_reviewer: Option<RawRequestedReviewer>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RawRollup {
    pub state: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RawRollupCommit {
    pub status_check_rollup: Option<RawRollup>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct RawCommitNode<C> {
    pub commit: C,
}

/// The head commit's rollup verdict, as a nameless check.
fn rollup_states(commits: &Connection<RawCommitNode<RawRollupCommit>>) -> Vec<RawCheck> {
    commits
        .nodes()
        .filter_map(|node| {
            let state = trimmed(node.commit.status_check_rollup.as_ref()?.state.as_deref())?;
            Some(RawCheck {
                state: Some(state),
                ..RawCheck::default()
            })
        })
        .collect()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRow {
    number: u64,
    title: String,
    url: String,
    #[serde(default)]
    author: Option<RawActor>,
    head_ref_name: String,
    base_ref_name: String,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(default)]
    review_decision: Option<String>,
    #[serde(default)]
    latest_reviews: Connection<RawLatestReview>,
    #[serde(default)]
    additions: Option<u64>,
    #[serde(default)]
    deletions: Option<u64>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    merged_at: Option<String>,
    #[serde(default)]
    review_requests: Connection<RawReviewRequest>,
    #[serde(default)]
    labels: Connection<RawLabel>,
    #[serde(default)]
    commits: Connection<RawCommitNode<RawRollupCommit>>,
}

/// One page of listing rows.
pub(crate) struct RowsPage {
    pub entries: Vec<ListEntry>,
    /// Rows GitHub sent, decodable or not, so paging does not stop early.
    pub raw_count: usize,
    pub next_cursor: Option<String>,
}

/// Rows of a `search` or `pullRequests` connection, each decoded on its own.
pub(crate) fn rows(connection: Option<Value>) -> RowsPage {
    let connection: Connection<Value> = connection
        .and_then(|c| serde_json::from_value(c).ok())
        .unwrap_or_default();
    let next_cursor = connection.next_cursor();
    let nodes: Vec<Value> = connection.into_nodes().collect();
    let raw_count = nodes.len();
    let entries = nodes
        .into_iter()
        .filter_map(|node| match serde_json::from_value::<RawRow>(node) {
            Ok(row) => Some(entry(row)),
            Err(e) => {
                tracing::debug!("Skipping an unreadable pull request row: {e}");
                None
            }
        })
        .collect();
    RowsPage {
        entries,
        raw_count,
        next_cursor,
    }
}

fn entry(row: RawRow) -> ListEntry {
    let requested: Vec<&RawRequestedReviewer> = row
        .review_requests
        .nodes()
        .filter_map(|r| r.requested_reviewer.as_ref())
        .collect();
    ListEntry {
        number: row.number,
        title: row.title,
        url: row.url,
        author: actor(row.author.as_ref()),
        head_branch: row.head_ref_name,
        base_branch: row.base_ref_name,
        state: state(
            row.state.as_deref(),
            trimmed(row.merged_at.as_deref()).is_some(),
        ),
        is_draft: row.is_draft,
        mergeability: mergeability(row.mergeable.as_deref()),
        review_decision: review_decision_with_reviews(
            row.review_decision.as_deref(),
            &row.latest_reviews,
        ),
        additions: row.additions.unwrap_or(0),
        deletions: row.deletions.unwrap_or(0),
        created_at: row.created_at,
        updated_at: row.updated_at,
        review_request_logins: requested
            .iter()
            .filter_map(|r| trimmed(r.login.as_deref()))
            .collect(),
        has_team_review_request: requested
            .iter()
            .any(|r| trimmed(r.login.as_deref()).is_none() && trimmed(r.slug.as_deref()).is_some()),
        labels: labels(&row.labels),
        checks_state: checks_state(&rollup_states(&row.commits)),
        stack: None,
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawSummary {
    number: u64,
    title: String,
    url: String,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(default)]
    review_decision: Option<String>,
    #[serde(default)]
    additions: u64,
    #[serde(default)]
    deletions: u64,
    #[serde(default)]
    changed_files: u64,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    merged_at: Option<String>,
    #[serde(default)]
    closed_at: Option<String>,
    head_ref_name: String,
    base_ref_name: String,
    #[serde(default)]
    author: Option<RawActor>,
    #[serde(default)]
    latest_reviews: Connection<RawLatestReview>,
    #[serde(default)]
    commits: Connection<RawCommitNode<RawRollupCommit>>,
}

fn optional_time(value: Option<&str>) -> Option<DateTime<Utc>> {
    trimmed(value).and_then(|v| DateTime::parse_from_rfc3339(&v).ok().map(|t| t.to_utc()))
}

pub(crate) fn summary(raw: RawSummary) -> Summary {
    Summary {
        number: raw.number,
        title: raw.title,
        url: raw.url,
        head_branch: raw.head_ref_name,
        base_branch: raw.base_ref_name,
        state: state(
            raw.state.as_deref(),
            trimmed(raw.merged_at.as_deref()).is_some(),
        ),
        is_draft: raw.is_draft,
        closed_at: optional_time(raw.closed_at.as_deref()),
        merged_at: optional_time(raw.merged_at.as_deref()),
        updated_at: raw.updated_at,
        author: actor(raw.author.as_ref()),
        additions: raw.additions,
        deletions: raw.deletions,
        changed_files: raw.changed_files,
        review_decision: review_decision_with_reviews(
            raw.review_decision.as_deref(),
            &raw.latest_reviews,
        ),
        checks_state: checks_state(&rollup_states(&raw.commits)),
        mergeability: mergeability(raw.mergeable.as_deref()),
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawNameWithOwner {
    name_with_owner: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawHeadNode {
    number: u64,
    title: String,
    url: String,
    base_ref_name: String,
    head_ref_name: String,
    #[serde(default)]
    head_ref_oid: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    merged_at: Option<String>,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    is_cross_repository: bool,
    #[serde(default)]
    head_repository: Option<RawNameWithOwner>,
}

pub(crate) fn head_pull_request(raw: RawHeadNode) -> HeadPullRequest {
    HeadPullRequest {
        number: raw.number,
        title: raw.title,
        url: raw.url,
        head_branch: raw.head_ref_name,
        base_branch: raw.base_ref_name,
        head_sha: trimmed(raw.head_ref_oid.as_deref()),
        state: state(
            raw.state.as_deref(),
            trimmed(raw.merged_at.as_deref()).is_some(),
        ),
        is_draft: raw.is_draft,
        updated_at: raw.updated_at,
        is_cross_repository: raw.is_cross_repository,
        head_repository: raw
            .head_repository
            .and_then(|r| trimmed(r.name_with_owner.as_deref())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ChecksState, PullRequestState, ReviewDecision};

    fn fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../tests/fixtures/pull_request_search.json"
        ))
        .unwrap()
    }

    #[test]
    fn reads_search_rows_into_entries() {
        let data = fixture();
        let page = rows(data["data"]["search"].clone().into());
        assert_eq!(page.next_cursor.as_deref(), Some("Y3Vyc29yOjI="));
        let first = &page.entries[0];
        assert_eq!(first.number, 290);
        assert_eq!(
            first.title,
            "feat(files): show raster images in the Files view"
        );
        assert_eq!(first.author.as_ref().unwrap().login, "stippi");
        assert_eq!(first.state, PullRequestState::Merged);
        assert_eq!(first.checks_state, Some(ChecksState::Passing));
        assert_eq!(first.review_decision, Some(ReviewDecision::Approved));
        assert_eq!(first.labels[0].name, "enhancement");
        assert_eq!(first.additions, 120);
        let second = &page.entries[1];
        assert!(second.is_draft);
        assert_eq!(second.review_request_logins, ["octocat"]);
        assert!(second.has_team_review_request);
        assert_eq!(second.checks_state, Some(ChecksState::Failing));
    }

    #[test]
    fn skips_malformed_rows_but_still_counts_them() {
        let data = fixture();
        let page = rows(data["data"]["search"].clone().into());
        // The third node lacks its number, and an empty node is not a pull request.
        assert_eq!(page.raw_count, 4);
        assert_eq!(page.entries.len(), 2);
    }
}
