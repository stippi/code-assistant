use super::PullRequestState;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckStatus {
    Pending,
    ActionRequired,
    Success,
    Failure,
    Skipped,
    Neutral,
    Cancelled,
}

/// One status or check run of the head commit. Several runs of the same
/// check are folded into the newest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// The check's name, prefixed with `workflow / ` when two workflows run
    /// a check of that name.
    pub name: String,
    pub status: CheckStatus,
    pub description: Option<String>,
    pub url: Option<String>,
    /// Required by branch protection; only known on github.com.
    pub required: Option<bool>,
}

/// The verdict over all checks of a head commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChecksState {
    Passing,
    Failing,
    Pending,
}

/// The checks of a pull request's head, as the checks poll reads them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checks {
    pub state: PullRequestState,
    pub head_sha: Option<String>,
    pub checks: Vec<Check>,
    pub checks_state: Option<ChecksState>,
}
