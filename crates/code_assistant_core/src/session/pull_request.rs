//! The pull request behind a session's branch, as the sidebar shows it.
//!
//! Read through the GitHub client in batches: one summary request covers
//! every session whose pull request is known, a lookup by branch finds the
//! others. Without a token (no `gh` login, no `GH_TOKEN`) the reads fail
//! and sessions simply show their branch without a pull request. The
//! snapshot is kept in the session's lifecycle record and refreshed by the
//! lifecycle sweep and after each run.

use std::time::SystemTime;

use github::model::Summary;
use github::{PullRequestRef, RepoRef};
use serde::{Deserialize, Serialize};

use crate::pull_requests::own_pull_request;

/// What the sidebar shows about a session's pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestSnapshot {
    pub number: u64,
    pub url: String,
    pub title: String,
    pub state: PullRequestState,
    pub review: Option<ReviewDecision>,
    pub checks: Option<ChecksState>,
    pub fetched_at: SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestState {
    Open,
    Draft,
    Merged,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChecksState {
    Passing,
    Failing,
    Pending,
}

impl PullRequestSnapshot {
    pub fn from_summary(summary: &Summary, fetched_at: SystemTime) -> Self {
        let state = match summary.state {
            github::model::PullRequestState::Open if summary.is_draft => PullRequestState::Draft,
            github::model::PullRequestState::Open => PullRequestState::Open,
            github::model::PullRequestState::Merged => PullRequestState::Merged,
            github::model::PullRequestState::Closed => PullRequestState::Closed,
        };
        Self {
            number: summary.number,
            url: summary.url.clone(),
            title: summary.title.clone(),
            state,
            review: summary.review_decision.map(|review| match review {
                github::model::ReviewDecision::Approved => ReviewDecision::Approved,
                github::model::ReviewDecision::ChangesRequested => ReviewDecision::ChangesRequested,
                github::model::ReviewDecision::ReviewRequired => ReviewDecision::ReviewRequired,
            }),
            checks: summary.checks_state.map(|checks| match checks {
                github::model::ChecksState::Passing => ChecksState::Passing,
                github::model::ChecksState::Failing => ChecksState::Failing,
                github::model::ChecksState::Pending => ChecksState::Pending,
            }),
            fetched_at,
        }
    }
}

/// A session's branch whose pull request is read.
#[derive(Debug, Clone)]
pub(crate) struct BranchLookup {
    pub repo: RepoRef,
    pub branch: String,
    /// The pull request last seen for the branch.
    pub known: Option<u64>,
}

/// The pull request of each branch, in order: `Ok(None)` when the branch
/// has none, an error when it could not be read. Known pull requests are
/// read in one batch; a branch whose known pull request is gone or now
/// has another head, and a branch without one, is looked up by name.
pub(crate) async fn read_branch_pull_requests(
    client: &github::Client,
    lookups: &[BranchLookup],
) -> Vec<github::Result<Option<PullRequestSnapshot>>> {
    let mut results: Vec<Option<github::Result<Option<PullRequestSnapshot>>>> =
        lookups.iter().map(|_| None).collect();
    let mut by_branch: Vec<usize> = Vec::new();

    let known: Vec<(usize, PullRequestRef)> = lookups
        .iter()
        .enumerate()
        .filter_map(|(index, lookup)| Some((index, lookup.repo.pull_request(lookup.known?))))
        .collect();
    for ((index, _), summary) in known.iter().zip(summaries(client, &known).await) {
        match summary {
            Ok(summary) if summary.head_branch == lookups[*index].branch => {
                results[*index] = Some(Ok(Some(PullRequestSnapshot::from_summary(
                    &summary,
                    SystemTime::now(),
                ))));
            }
            Ok(_) => by_branch.push(*index),
            Err(e) if e.is_not_found() => by_branch.push(*index),
            Err(e) => results[*index] = Some(Err(e)),
        }
    }
    by_branch.extend(
        lookups
            .iter()
            .enumerate()
            .filter(|(_, lookup)| lookup.known.is_none())
            .map(|(index, _)| index),
    );

    let found = futures::future::join_all(by_branch.iter().map(|&index| {
        let lookup = &lookups[index];
        client.pull_requests_for_head(&lookup.repo, &lookup.branch)
    }))
    .await;
    let mut numbered: Vec<(usize, PullRequestRef)> = Vec::new();
    for (index, found) in by_branch.into_iter().zip(found) {
        match found.map(|found| own_pull_request(&found)) {
            Ok(Some(number)) => numbered.push((index, lookups[index].repo.pull_request(number))),
            Ok(None) => results[index] = Some(Ok(None)),
            Err(e) => results[index] = Some(Err(e)),
        }
    }
    for ((index, _), summary) in numbered.iter().zip(summaries(client, &numbered).await) {
        results[*index] = Some(summary.map(|summary| {
            Some(PullRequestSnapshot::from_summary(
                &summary,
                SystemTime::now(),
            ))
        }));
    }

    results
        .into_iter()
        .map(|result| result.expect("every branch was read"))
        .collect()
}

async fn summaries(
    client: &github::Client,
    prs: &[(usize, PullRequestRef)],
) -> Vec<github::Result<Summary>> {
    if prs.is_empty() {
        return Vec::new();
    }
    let prs: Vec<PullRequestRef> = prs.iter().map(|(_, pr)| pr.clone()).collect();
    client.summaries(&prs).await
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use github::fake::{FakeResponse, FakeTransport};
    use serde_json::{Value, json};

    /// A `PullRequestSummary` node as GitHub answers it.
    pub(crate) fn summary_node(number: u64, head: &str, state: &str) -> Value {
        json!({
            "number": number,
            "title": format!("PR {number}"),
            "url": format!("https://github.com/stippi/code-assistant/pull/{number}"),
            "state": state,
            "isDraft": false,
            "mergeable": "MERGEABLE",
            "reviewDecision": "APPROVED",
            "additions": 1,
            "deletions": 2,
            "changedFiles": 3,
            "updatedAt": "2026-10-30T10:00:00Z",
            "mergedAt": null,
            "closedAt": null,
            "headRefName": head,
            "baseRefName": "main",
            "author": {"__typename": "User", "login": "stippi"},
            "latestReviews": {"nodes": []},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "SUCCESS"}}}]}
        })
    }

    /// A `PullRequestsByHead` answer listing `numbers`, newest first.
    pub(crate) fn head_nodes(head: &str, numbers: &[u64]) -> Value {
        let nodes: Vec<Value> = numbers
            .iter()
            .map(|number| {
                json!({
                    "number": number, "title": format!("PR {number}"),
                    "url": format!("https://github.com/stippi/code-assistant/pull/{number}"),
                    "baseRefName": "main", "headRefName": head,
                    "headRefOid": "0123456789abcdef0123456789abcdef01234567",
                    "state": "OPEN", "isDraft": false, "mergedAt": null, "closedAt": null,
                    "updatedAt": "2026-10-30T10:00:00Z", "isCrossRepository": false,
                    "headRepository": {"name": "code-assistant", "nameWithOwner": "stippi/code-assistant"},
                    "headRepositoryOwner": {"login": "stippi"}
                })
            })
            .collect();
        json!({"repository": {"pullRequests": {"nodes": nodes}}})
    }

    fn lookup(branch: &str, known: Option<u64>) -> BranchLookup {
        BranchLookup {
            repo: RepoRef::new("github.com", "stippi", "code-assistant"),
            branch: branch.into(),
            known,
        }
    }

    #[tokio::test]
    async fn known_pull_requests_are_read_in_one_batch_and_the_rest_by_branch() {
        let fake = FakeTransport::new();
        // 5 has moved to another head, 6 is gone, 7 is current.
        fake.on_graphql("PullRequestSummary")
            .with_variables(json!({"number_0": 5}))
            .respond(FakeResponse::json(
                200,
                json!({
                    "data": {
                        "a0_repository": {"pullRequest": summary_node(5, "elsewhere", "OPEN")},
                        "a1_repository": {"pullRequest": null},
                        "a2_repository": {"pullRequest": summary_node(7, "feature/c", "MERGED")}
                    },
                    "errors": [{"type": "NOT_FOUND", "path": ["a1_repository", "pullRequest"],
                                "message": "Could not resolve to a PullRequest"}]
                }),
            ));
        fake.on_graphql("PullRequestsByHead")
            .with_variables(json!({"head": "feature/a"}))
            .data(head_nodes("feature/a", &[9]));
        fake.on_graphql("PullRequestsByHead")
            .with_variables(json!({"head": "feature/b"}))
            .data(head_nodes("feature/b", &[]));
        fake.on_graphql("PullRequestsByHead")
            .with_variables(json!({"head": "feature/d"}))
            .data(head_nodes("feature/d", &[10]));
        fake.on_graphql("PullRequestSummary")
            .with_variables(json!({"number_0": 9}))
            .data(json!({
                "a0_repository": {"pullRequest": summary_node(9, "feature/a", "OPEN")},
                "a1_repository": {"pullRequest": summary_node(10, "feature/d", "OPEN")}
            }));

        let read = read_branch_pull_requests(
            &fake.client(),
            &[
                lookup("feature/a", Some(5)),
                lookup("feature/b", Some(6)),
                lookup("feature/c", Some(7)),
                lookup("feature/d", None),
            ],
        )
        .await;

        let numbers: Vec<Option<u64>> = read
            .iter()
            .map(|r| r.as_ref().unwrap().as_ref().map(|pr| pr.number))
            .collect();
        assert_eq!(numbers, [Some(9), None, Some(7), Some(10)]);
        let merged = read[2].as_ref().unwrap().as_ref().unwrap();
        assert_eq!(merged.state, PullRequestState::Merged);
        assert_eq!(merged.review, Some(ReviewDecision::Approved));
        assert_eq!(merged.checks, Some(ChecksState::Passing));
        assert_eq!(
            fake.operations(),
            [
                "PullRequestSummary",
                "PullRequestsByHead",
                "PullRequestsByHead",
                "PullRequestsByHead",
                "PullRequestSummary"
            ]
        );
    }

    #[tokio::test]
    async fn without_a_credential_every_branch_fails_and_nothing_is_guessed() {
        struct NoToken;
        #[async_trait::async_trait]
        impl github::TokenSource for NoToken {
            async fn token(&self, host: &str) -> github::Result<String> {
                Err(github::Error::NoCredential {
                    host: host.into(),
                    detail: "gh is not installed".into(),
                })
            }
        }
        let fake = FakeTransport::new();
        let client = github::Client::with_parts(fake.clone(), std::sync::Arc::new(NoToken));

        let read =
            read_branch_pull_requests(&client, &[lookup("a", Some(1)), lookup("b", None)]).await;

        assert!(
            read.iter()
                .all(|r| matches!(r, Err(github::Error::NoCredential { .. })))
        );
        assert!(fake.requests().is_empty());
    }

    #[test]
    fn an_open_draft_reads_as_a_draft() {
        let mut summary: Summary = serde_json::from_value(json!({
            "number": 1, "title": "t", "url": "u", "head_branch": "h", "base_branch": "main",
            "state": "open", "is_draft": true, "closed_at": null, "merged_at": null,
            "updated_at": "2026-10-30T10:00:00Z", "author": null, "additions": 0,
            "deletions": 0, "changed_files": 0, "review_decision": null,
            "checks_state": "pending", "mergeability": "unknown"
        }))
        .unwrap();
        let snapshot = PullRequestSnapshot::from_summary(&summary, SystemTime::UNIX_EPOCH);
        assert_eq!(snapshot.state, PullRequestState::Draft);
        assert_eq!(snapshot.review, None);
        assert_eq!(snapshot.checks, Some(ChecksState::Pending));
        summary.state = github::model::PullRequestState::Closed;
        let closed = PullRequestSnapshot::from_summary(&summary, SystemTime::UNIX_EPOCH);
        assert_eq!(closed.state, PullRequestState::Closed);
    }
}
