//! Workflow runs of a pull request from a fork that wait for a maintainer
//! to approve them, and approving them. Ported from T3 Code: the runs show
//! as checks ("Awaiting approval"), and approving re-reads the head before
//! each run so a push made meanwhile is never approved by accident.

use super::ensure_addressable;
use crate::PullRequestRef;
use crate::client::{Client, RestCall};
use crate::error::{Error, Result};
use crate::model::{Check, CheckStatus, ChecksState, Detail, PullRequestState};
use crate::transport::Method;
use serde::Deserialize;

const RUNS_PAGE_SIZE: usize = 100;
/// Runs read at most; past it, approving is refused.
const RUNS_LIMIT: usize = 1000;

/// A workflow run waiting for approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkflowRun {
    pub id: u64,
    pub name: String,
    pub url: Option<String>,
}

/// The id in a workflow run's URL (`…/actions/runs/123…`).
pub(crate) fn run_id_of(url: &str) -> Option<u64> {
    let rest = &url[url.find("/actions/runs/")? + "/actions/runs/".len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The head a fork's pull request is approved for.
struct ForkHead<'a> {
    sha: &'a str,
    branch: &'a str,
    owner: &'a str,
}

fn fork_head(detail: &Detail) -> Option<ForkHead<'_>> {
    Some(ForkHead {
        sha: detail.head_sha.as_deref()?,
        branch: Some(detail.head_branch.as_str()).filter(|b| !b.is_empty())?,
        owner: detail.head_repository_owner.as_deref()?,
    })
}

/// Add the runs awaiting approval to `detail`'s checks (those its checks do
/// not show already) and count them; `None` leaves the count unknown and
/// says so with a check of its own.
pub(crate) fn apply_workflow_runs(detail: &mut Detail, runs: Option<&[WorkflowRun]>) {
    let Some(runs) = runs else {
        detail.workflow_approvals = None;
        detail.checks.push(Check {
            name: "Workflow approval status".into(),
            status: CheckStatus::ActionRequired,
            description: Some(
                "GitHub could not determine whether workflows are awaiting approval.".into(),
            ),
            url: None,
            required: None,
        });
        detail.checks_state = merged_state(detail.checks_state);
        return;
    };
    detail.workflow_approvals = Some(runs.len() as u64);
    let shown: Vec<u64> = detail
        .checks
        .iter()
        .filter(|check| check.status == CheckStatus::ActionRequired)
        .filter_map(|check| check.url.as_deref().and_then(run_id_of))
        .collect();
    let mut added = false;
    for run in runs.iter().filter(|run| !shown.contains(&run.id)) {
        detail.checks.push(Check {
            name: run.name.clone(),
            status: CheckStatus::ActionRequired,
            description: Some("A maintainer must approve this workflow before it can run.".into()),
            url: run.url.clone(),
            required: None,
        });
        added = true;
    }
    if added {
        detail.checks_state = merged_state(detail.checks_state);
    }
}

/// The verdict once a check awaiting action joins: pending unless failing.
fn merged_state(state: Option<ChecksState>) -> Option<ChecksState> {
    match state {
        Some(ChecksState::Failing) => Some(ChecksState::Failing),
        _ => Some(ChecksState::Pending),
    }
}

impl Client {
    /// The runs of `head` on `branch` that wait for approval.
    pub(crate) async fn workflow_runs_awaiting_approval(
        &self,
        pr: &PullRequestRef,
        head: &str,
        branch: &str,
    ) -> Result<Vec<WorkflowRun>> {
        ensure_addressable(&pr.repo)?;
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct RawRun {
            id: Option<u64>,
            name: Option<String>,
            html_url: Option<String>,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Runs {
            workflow_runs: Vec<RawRun>,
        }
        let mut runs = Vec::new();
        for page in 1.. {
            let path = format!(
                "repos/{}/{}/actions/runs?head_sha={}&branch={}&event=pull_request&status=action_required&per_page={RUNS_PAGE_SIZE}&page={page}",
                pr.repo.owner,
                pr.repo.name,
                urlencoding::encode(head),
                urlencoding::encode(branch),
            );
            let response = self
                .rest(RestCall::get(&pr.repo.host, "workflowRuns", path))
                .await?;
            let raw: Runs = serde_json::from_slice(&response.body)
                .map_err(|e| Error::decode("workflowRuns", e))?;
            let count = raw.workflow_runs.len();
            runs.extend(raw.workflow_runs.into_iter().filter_map(|run| {
                let id = run.id?;
                Some(WorkflowRun {
                    id,
                    name: run
                        .name
                        .map(|name| name.trim().to_string())
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| format!("Workflow run {id}")),
                    url: run.html_url.filter(|url| !url.trim().is_empty()),
                })
            }));
            if count < RUNS_PAGE_SIZE {
                break;
            }
            if runs.len() >= RUNS_LIMIT {
                return Err(Error::Refused(
                    "GitHub returned more than 1000 workflow runs awaiting approval.".into(),
                ));
            }
        }
        Ok(runs)
    }

    /// For an open pull request from a fork: its runs awaiting approval,
    /// added to its checks. A failed read leaves the count unknown, unless
    /// GitHub limited the rate (that fails the read).
    pub(crate) async fn add_workflow_approvals(
        &self,
        pr: &PullRequestRef,
        detail: &mut Detail,
    ) -> Result<()> {
        if detail.state != PullRequestState::Open || !detail.is_cross_repository {
            return Ok(());
        }
        let runs = match fork_head(detail) {
            Some(head) => {
                match self
                    .workflow_runs_awaiting_approval(pr, head.sha, head.branch)
                    .await
                {
                    Ok(runs) => Some(runs),
                    Err(e) if e.is_rate_limited() => return Err(e),
                    Err(e) => {
                        tracing::debug!("workflow runs awaiting approval not read: {e}");
                        None
                    }
                }
            }
            None => None,
        };
        apply_workflow_runs(detail, runs.as_deref());
        Ok(())
    }

    /// Approve the workflow runs of a pull request from a fork. Before
    /// each run the head is read again: a new push, or a head that names
    /// another pull request too, is refused rather than approved.
    pub(crate) async fn approve_workflows(&self, pr: &PullRequestRef) -> Result<()> {
        let detail = self.detail_without_approvals(pr).await?;
        if !detail.is_cross_repository {
            return Ok(());
        }
        let unavailable = || {
            Error::Refused(format!(
                "GitHub did not report a complete head revision for #{}.",
                pr.number
            ))
        };
        let head = fork_head(&detail).ok_or_else(unavailable)?;
        let (sha, branch, owner) = (
            head.sha.to_string(),
            head.branch.to_string(),
            head.owner.to_string(),
        );
        self.ensure_unique_head(pr, &sha, &branch, &owner).await?;
        let runs = self
            .workflow_runs_awaiting_approval(pr, &sha, &branch)
            .await?;
        for run in runs {
            let now = self.detail_without_approvals(pr).await?;
            let same = now.is_cross_repository
                && now.head_sha.as_deref() == Some(sha.as_str())
                && now.head_branch == branch
                && now
                    .head_repository_owner
                    .as_deref()
                    .is_some_and(|o| o.eq_ignore_ascii_case(&owner));
            if !same {
                return Err(Error::Refused(format!(
                    "The head revision of #{} changed before its workflows could be approved.",
                    pr.number
                )));
            }
            let still = self
                .workflow_runs_awaiting_approval(pr, &sha, &branch)
                .await?;
            if !still.iter().any(|r| r.id == run.id) {
                continue;
            }
            self.rest(RestCall::new(
                &pr.repo.host,
                "approveWorkflowRun",
                Method::Post,
                format!(
                    "repos/{}/{}/actions/runs/{}/approve",
                    pr.repo.owner, pr.repo.name, run.id
                ),
            ))
            .await?;
        }
        Ok(())
    }

    /// Exactly one open pull request from that fork may have this head:
    /// approving runs another one's workflows too otherwise.
    async fn ensure_unique_head(
        &self,
        pr: &PullRequestRef,
        sha: &str,
        branch: &str,
        owner: &str,
    ) -> Result<()> {
        let heads = self.pull_requests_for_head(&pr.repo, branch).await?;
        let matching: Vec<u64> = heads
            .iter()
            .filter(|head| {
                head.state == PullRequestState::Open
                    && head.is_cross_repository
                    && head.head_sha.as_deref() == Some(sha)
                    && head
                        .head_repository
                        .as_deref()
                        .and_then(|r| r.split('/').next())
                        .is_some_and(|o| o.eq_ignore_ascii_case(owner))
            })
            .map(|head| head.number)
            .collect();
        if matching != [pr.number] {
            return Err(Error::Refused(format!(
                "The head revision matched {} pull requests instead of uniquely matching #{}.",
                matching.len(),
                pr.number
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_run_id_from_its_url() {
        assert_eq!(
            run_id_of("https://github.com/o/n/actions/runs/123/job/4"),
            Some(123)
        );
        assert_eq!(
            run_id_of("https://github.com/o/n/actions/runs/77"),
            Some(77)
        );
        assert_eq!(run_id_of("https://github.com/o/n/runs/77"), None);
        assert_eq!(run_id_of("https://github.com/o/n/actions/runs/x"), None);
    }
}
