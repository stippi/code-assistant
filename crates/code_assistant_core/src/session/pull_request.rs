//! The pull request behind a session's branch, read through the GitHub CLI.
//!
//! `gh pr view <branch>` is the only dependency: when `gh` is missing or not
//! logged in, sessions simply show their branch without a pull request. The
//! snapshot is kept in the session's lifecycle record and refreshed by the
//! lifecycle sweep and after each run.

use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

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

/// The fields asked of `gh pr view --json`.
const JSON_FIELDS: &str = "number,url,title,state,isDraft,reviewDecision,statusCheckRollup";

const GH_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a pull request could not be read.
#[derive(Debug)]
pub enum PullRequestError {
    /// `gh` is not installed; nothing on this machine will answer.
    GhUnavailable,
    Other(anyhow::Error),
}

impl std::fmt::Display for PullRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GhUnavailable => write!(f, "the GitHub CLI (gh) is not installed"),
            Self::Other(e) => write!(f, "{e:#}"),
        }
    }
}

/// The pull request whose head is `branch`, or `None` when there is none.
pub async fn fetch_pull_request(
    repo_root: &Path,
    branch: &str,
) -> std::result::Result<Option<PullRequestSnapshot>, PullRequestError> {
    let output = tokio::time::timeout(
        GH_TIMEOUT,
        tokio::process::Command::new("gh")
            .args(["pr", "view", branch, "--json", JSON_FIELDS])
            .current_dir(repo_root)
            .output(),
    )
    .await
    .map_err(|_| PullRequestError::Other(anyhow::anyhow!("gh pr view timed out")))?;
    let output = match output {
        Ok(output) => output,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(PullRequestError::GhUnavailable);
        }
        Err(e) => return Err(PullRequestError::Other(e.into())),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("no pull requests found") {
            return Ok(None);
        }
        return Err(PullRequestError::Other(anyhow::anyhow!(
            "gh pr view {branch} failed: {}",
            stderr.trim()
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_pr_view(&stdout, SystemTime::now())
        .map(Some)
        .map_err(PullRequestError::Other)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrView {
    number: u64,
    url: String,
    #[serde(default)]
    title: String,
    state: String,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    review_decision: String,
    #[serde(default)]
    status_check_rollup: Vec<CheckRollupItem>,
}

/// One entry of `statusCheckRollup`: a check run (`status` + `conclusion`)
/// or a commit status context (`state`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckRollupItem {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

/// Parse the JSON of `gh pr view --json` with [`JSON_FIELDS`].
pub fn parse_pr_view(json: &str, fetched_at: SystemTime) -> Result<PullRequestSnapshot> {
    let view: PrView = serde_json::from_str(json).context("unexpected gh pr view output")?;
    let state = match (view.state.as_str(), view.is_draft) {
        ("OPEN", true) => PullRequestState::Draft,
        ("OPEN", false) => PullRequestState::Open,
        ("MERGED", _) => PullRequestState::Merged,
        ("CLOSED", _) => PullRequestState::Closed,
        (other, _) => bail!("unknown pull request state {other:?}"),
    };
    let review = match view.review_decision.as_str() {
        "APPROVED" => Some(ReviewDecision::Approved),
        "CHANGES_REQUESTED" => Some(ReviewDecision::ChangesRequested),
        "REVIEW_REQUIRED" => Some(ReviewDecision::ReviewRequired),
        _ => None,
    };
    Ok(PullRequestSnapshot {
        number: view.number,
        url: view.url,
        title: view.title,
        state,
        review,
        checks: summarize_checks(&view.status_check_rollup),
        fetched_at,
    })
}

/// One verdict for the whole rollup: any failure fails it, any unfinished
/// check leaves it pending, otherwise it passes. No checks, no verdict.
fn summarize_checks(items: &[CheckRollupItem]) -> Option<ChecksState> {
    if items.is_empty() {
        return None;
    }
    let mut pending = false;
    for item in items {
        let verdict = item
            .conclusion
            .as_deref()
            .filter(|c| !c.is_empty())
            .or(item.state.as_deref());
        match verdict {
            Some(
                "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED"
                | "STARTUP_FAILURE",
            ) => {
                return Some(ChecksState::Failing);
            }
            Some("SUCCESS" | "NEUTRAL" | "SKIPPED") => {}
            // No conclusion yet, or a status context still pending/expected.
            _ => pending = true,
        }
        if item.status.as_deref().is_some_and(|s| s != "COMPLETED") {
            pending = true;
        }
    }
    Some(if pending {
        ChecksState::Pending
    } else {
        ChecksState::Passing
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MERGED: &str = r#"{"number":219,"url":"https://github.com/o/r/pull/219","title":"Record a tab",
        "state":"MERGED","isDraft":false,"reviewDecision":"",
        "statusCheckRollup":[
          {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS","name":"a"},
          {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS","name":"b"}]}"#;

    #[test]
    fn a_merged_pull_request_with_green_checks() {
        let pr = parse_pr_view(MERGED, SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(pr.number, 219);
        assert_eq!(pr.state, PullRequestState::Merged);
        assert_eq!(pr.review, None);
        assert_eq!(pr.checks, Some(ChecksState::Passing));
    }

    #[test]
    fn an_open_draft_with_a_running_check_is_pending() {
        let json = r#"{"number":1,"url":"u","title":"t","state":"OPEN","isDraft":true,
            "reviewDecision":"REVIEW_REQUIRED",
            "statusCheckRollup":[{"__typename":"CheckRun","status":"IN_PROGRESS","conclusion":""}]}"#;
        let pr = parse_pr_view(json, SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(pr.state, PullRequestState::Draft);
        assert_eq!(pr.review, Some(ReviewDecision::ReviewRequired));
        assert_eq!(pr.checks, Some(ChecksState::Pending));
    }

    #[test]
    fn one_failing_status_context_fails_the_rollup() {
        let json = r#"{"number":2,"url":"u","title":"t","state":"OPEN","isDraft":false,
            "reviewDecision":"CHANGES_REQUESTED",
            "statusCheckRollup":[
              {"__typename":"StatusContext","state":"SUCCESS","context":"ci"},
              {"__typename":"StatusContext","state":"FAILURE","context":"lint"}]}"#;
        let pr = parse_pr_view(json, SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(pr.state, PullRequestState::Open);
        assert_eq!(pr.review, Some(ReviewDecision::ChangesRequested));
        assert_eq!(pr.checks, Some(ChecksState::Failing));
    }

    #[test]
    fn no_checks_means_no_verdict_and_closed_is_closed() {
        let json = r#"{"number":3,"url":"u","title":"t","state":"CLOSED","isDraft":false,
            "reviewDecision":"","statusCheckRollup":[]}"#;
        let pr = parse_pr_view(json, SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(pr.state, PullRequestState::Closed);
        assert_eq!(pr.checks, None);
    }

    #[test]
    fn an_unknown_state_is_an_error() {
        let json = r#"{"number":3,"url":"u","title":"t","state":"WEIRD","isDraft":false}"#;
        assert!(parse_pr_view(json, SystemTime::UNIX_EPOCH).is_err());
    }
}
