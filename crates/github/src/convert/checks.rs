//! Check runs and commit statuses → `Check`s and a verdict.
//!
//! The rollup repeats a check for every run (re-runs, push and pull_request
//! workflows), and runs carry no stable id. A check is identified by
//! workflow plus name; the newest run wins, a running one over any finished.

use super::trimmed;
use crate::model::{Check, CheckStatus, ChecksState};
use chrono::DateTime;
use serde::Deserialize;
use std::collections::HashMap;

/// A rollup context: a check run (status + conclusion) or a commit status
/// (state). Also what the listings' rollup `state` is read into.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RawCheck {
    pub name: Option<String>,
    pub context: Option<String>,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    pub state: Option<String>,
    pub description: Option<String>,
    pub details_url: Option<String>,
    pub target_url: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub is_required: Option<bool>,
    pub check_suite: Option<RawCheckSuite>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RawCheckSuite {
    workflow_run: Option<RawWorkflowRun>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RawWorkflowRun {
    workflow: Option<RawWorkflow>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RawWorkflow {
    name: Option<String>,
}

/// GitHub's sentinel for a time not reached yet.
const UNSET_TIMESTAMP: &str = "0001-01-01T00:00:00Z";

impl RawCheck {
    fn display_name(&self) -> Option<String> {
        trimmed(self.name.as_deref()).or_else(|| trimmed(self.context.as_deref()))
    }

    fn workflow_name(&self) -> Option<String> {
        let run = self.check_suite.as_ref()?.workflow_run.as_ref()?;
        trimmed(run.workflow.as_ref()?.name.as_deref())
    }

    fn status(&self) -> CheckStatus {
        if let Some(status) = trimmed(self.status.as_deref())
            && !status.eq_ignore_ascii_case("COMPLETED")
        {
            return CheckStatus::Pending;
        }
        let verdict = self.conclusion.as_deref().or(self.state.as_deref());
        match verdict.map(|v| v.trim().to_ascii_uppercase()).as_deref() {
            Some("SUCCESS") => CheckStatus::Success,
            Some("ACTION_REQUIRED") => CheckStatus::ActionRequired,
            Some("FAILURE" | "ERROR" | "TIMED_OUT" | "STARTUP_FAILURE") => CheckStatus::Failure,
            Some("CANCELLED") => CheckStatus::Cancelled,
            Some("SKIPPED") => CheckStatus::Skipped,
            Some("PENDING" | "EXPECTED") => CheckStatus::Pending,
            _ => CheckStatus::Neutral,
        }
    }
}

/// When a run happened, for picking the newest of a check's runs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RunTime {
    Running,
    At(String),
    Unknown,
}

pub(crate) struct CheckEntry {
    pub check: Check,
    pub workflow_name: Option<String>,
    pub at: RunTime,
}

fn real_timestamp(value: Option<&str>) -> Option<String> {
    trimmed(value).filter(|at| at != UNSET_TIMESTAMP)
}

fn entries(raw: &[RawCheck]) -> Vec<CheckEntry> {
    raw.iter()
        .filter_map(|check| {
            let name = check.display_name()?;
            let status = check.status();
            let at = if status == CheckStatus::Pending {
                RunTime::Running
            } else {
                real_timestamp(check.completed_at.as_deref())
                    .or_else(|| real_timestamp(check.started_at.as_deref()))
                    .map_or(RunTime::Unknown, RunTime::At)
            };
            Some(CheckEntry {
                check: Check {
                    name,
                    status,
                    description: trimmed(check.description.as_deref()),
                    url: trimmed(check.details_url.as_deref())
                        .or_else(|| trimmed(check.target_url.as_deref())),
                    required: check.is_required,
                },
                workflow_name: check.workflow_name(),
                at,
            })
        })
        .collect()
}

/// Whether `candidate` replaces `kept`: it is at least as new. Instants are
/// compared as times where both parse, else as text.
fn is_at_least_as_new(candidate: &RunTime, kept: &RunTime) -> bool {
    match (candidate, kept) {
        (RunTime::Running, _) => true,
        (_, RunTime::Running) => false,
        (RunTime::Unknown, kept) => *kept == RunTime::Unknown,
        (RunTime::At(_), RunTime::Unknown) => true,
        (RunTime::At(candidate), RunTime::At(kept)) => {
            match (
                DateTime::parse_from_rfc3339(candidate),
                DateTime::parse_from_rfc3339(kept),
            ) {
                (Ok(candidate), Ok(kept)) => candidate >= kept,
                _ => candidate >= kept,
            }
        }
    }
}

/// One check per workflow and name, the newest run kept at the place the
/// check first appeared; failed and cancelled checks first. A name two
/// workflows share is prefixed with its workflow.
pub(crate) fn dedupe(entries: Vec<CheckEntry>) -> Vec<Check> {
    let mut kept: Vec<CheckEntry> = Vec::new();
    let mut index_by_key: HashMap<(String, String), usize> = HashMap::new();
    for entry in entries {
        let key = (
            entry.workflow_name.clone().unwrap_or_default(),
            entry.check.name.clone(),
        );
        match index_by_key.get(&key) {
            Some(&index) => {
                if is_at_least_as_new(&entry.at, &kept[index].at) {
                    kept[index] = entry;
                }
            }
            None => {
                index_by_key.insert(key, kept.len());
                kept.push(entry);
            }
        }
    }
    let mut count_by_name: HashMap<String, usize> = HashMap::new();
    for entry in &kept {
        *count_by_name.entry(entry.check.name.clone()).or_default() += 1;
    }
    let mut checks: Vec<Check> = kept
        .into_iter()
        .map(|entry| {
            let workflow = entry.workflow_name.unwrap_or_default();
            if !workflow.is_empty() && count_by_name[&entry.check.name] > 1 {
                Check {
                    name: format!("{workflow} / {}", entry.check.name),
                    ..entry.check
                }
            } else {
                entry.check
            }
        })
        .collect();
    // Stable: each group keeps its order.
    checks.sort_by_key(|check| !is_failed(check.status));
    checks
}

fn is_failed(status: CheckStatus) -> bool {
    matches!(status, CheckStatus::Failure | CheckStatus::Cancelled)
}

/// The deduplicated checks of a rollup.
pub(crate) fn checks(raw: &[RawCheck]) -> Vec<Check> {
    dedupe(entries(raw))
}

/// The verdict over a rollup: failing if anything failed, pending while
/// anything runs or waits, passing once something passed. Nameless contexts
/// (as the listings' bare rollup `state`) count too.
pub(crate) fn checks_state(raw: &[RawCheck]) -> Option<ChecksState> {
    let statuses: Vec<CheckStatus> = checks(raw)
        .iter()
        .map(|check| check.status)
        .chain(
            raw.iter()
                .filter(|check| check.display_name().is_none())
                .map(RawCheck::status),
        )
        .collect();
    if statuses.iter().any(|s| is_failed(*s)) {
        Some(ChecksState::Failing)
    } else if statuses
        .iter()
        .any(|s| matches!(s, CheckStatus::Pending | CheckStatus::ActionRequired))
    {
        Some(ChecksState::Pending)
    } else if statuses.contains(&CheckStatus::Success) {
        Some(ChecksState::Passing)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        name: &str,
        status: CheckStatus,
        workflow: Option<&str>,
        at: Option<&str>,
    ) -> CheckEntry {
        CheckEntry {
            check: Check {
                name: name.into(),
                status,
                description: None,
                url: None,
                required: None,
            },
            workflow_name: workflow.map(str::to_string),
            at: at.map_or(RunTime::Unknown, |at| RunTime::At(at.into())),
        }
    }

    fn names_and_statuses(checks: &[Check]) -> Vec<(String, CheckStatus)> {
        checks.iter().map(|c| (c.name.clone(), c.status)).collect()
    }

    #[test]
    fn keeps_the_newer_instant_in_either_listing_order() {
        for (older, newer) in [
            ("2026-10-16T12:00:00+02:00", "2026-10-16T10:30:00Z"),
            ("2026-10-16T12:00:00.00009Z", "2026-10-16T12:00:00.0001Z"),
        ] {
            let forward = dedupe(vec![
                entry("build", CheckStatus::Success, None, Some(older)),
                entry("build", CheckStatus::Failure, None, Some(newer)),
            ]);
            let reversed = dedupe(vec![
                entry("build", CheckStatus::Failure, None, Some(newer)),
                entry("build", CheckStatus::Success, None, Some(older)),
            ]);
            assert_eq!(forward[0].status, CheckStatus::Failure, "{older} {newer}");
            assert_eq!(reversed[0].status, CheckStatus::Failure, "{older} {newer}");
        }
    }

    #[test]
    fn takes_the_last_copy_for_equal_instants() {
        let checks = dedupe(vec![
            entry(
                "build",
                CheckStatus::Success,
                None,
                Some("2026-10-16T12:00:00Z"),
            ),
            entry(
                "build",
                CheckStatus::Failure,
                None,
                Some("2026-10-16T14:00:00+02:00"),
            ),
        ]);
        assert_eq!(checks[0].status, CheckStatus::Failure);
    }

    #[test]
    fn holds_a_check_where_it_first_appeared_so_a_rerun_does_not_reshuffle() {
        let checks = dedupe(vec![
            entry(
                "lint",
                CheckStatus::Success,
                Some("CI"),
                Some("2026-08-11T16:00:00Z"),
            ),
            entry(
                "test",
                CheckStatus::Success,
                Some("CI"),
                Some("2026-08-11T16:00:00Z"),
            ),
            entry(
                "lint",
                CheckStatus::Success,
                Some("CI"),
                Some("2026-08-11T18:00:00Z"),
            ),
        ]);
        assert_eq!(
            names_and_statuses(&checks),
            [
                ("lint".into(), CheckStatus::Success),
                ("test".into(), CheckStatus::Success)
            ]
        );
    }

    #[test]
    fn puts_failed_and_cancelled_first_keeping_each_groups_order() {
        let checks = dedupe(vec![
            entry(
                "lint",
                CheckStatus::Failure,
                None,
                Some("2026-08-11T16:00:00Z"),
            ),
            entry("integration", CheckStatus::Cancelled, None, None),
            entry("types", CheckStatus::Pending, None, None),
            entry("unit", CheckStatus::Failure, None, None),
            entry("format", CheckStatus::Success, None, None),
            entry(
                "lint",
                CheckStatus::Success,
                None,
                Some("2026-08-11T18:00:00Z"),
            ),
        ]);
        assert_eq!(
            names_and_statuses(&checks),
            [
                ("integration".into(), CheckStatus::Cancelled),
                ("unit".into(), CheckStatus::Failure),
                ("lint".into(), CheckStatus::Success),
                ("types".into(), CheckStatus::Pending),
                ("format".into(), CheckStatus::Success),
            ]
        );
    }

    #[test]
    fn a_dated_run_beats_an_undated_one_whichever_came_first() {
        let undated_last = dedupe(vec![
            entry(
                "build",
                CheckStatus::Success,
                None,
                Some("2026-08-11T16:00:00Z"),
            ),
            entry("build", CheckStatus::Neutral, None, None),
        ]);
        let undated_first = dedupe(vec![
            entry("build", CheckStatus::Neutral, None, None),
            entry(
                "build",
                CheckStatus::Success,
                None,
                Some("2026-08-11T16:00:00Z"),
            ),
        ]);
        assert_eq!(undated_last[0].status, CheckStatus::Success);
        assert_eq!(undated_first[0].status, CheckStatus::Success);
    }

    #[test]
    fn a_running_check_outranks_every_finished_run() {
        let mut running = entry("build", CheckStatus::Pending, None, None);
        running.at = RunTime::Running;
        let checks = dedupe(vec![
            running,
            entry(
                "build",
                CheckStatus::Success,
                None,
                Some("2026-08-11T16:00:00Z"),
            ),
        ]);
        assert_eq!(checks[0].status, CheckStatus::Pending);
    }

    #[test]
    fn names_the_workflow_when_two_workflows_share_a_job_name() {
        let checks = dedupe(vec![
            entry(
                "build",
                CheckStatus::Success,
                Some("CI"),
                Some("2026-08-11T16:00:00Z"),
            ),
            entry(
                "build",
                CheckStatus::Failure,
                Some("Release"),
                Some("2026-08-11T16:00:00Z"),
            ),
            entry("build", CheckStatus::Success, Some(""), None),
        ]);
        assert_eq!(
            names_and_statuses(&checks),
            [
                ("Release / build".into(), CheckStatus::Failure),
                ("CI / build".into(), CheckStatus::Success),
                ("build".into(), CheckStatus::Success),
            ]
        );
    }

    fn raw(json: serde_json::Value) -> Vec<RawCheck> {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn maps_run_status_conclusion_and_status_state() {
        let checks = checks(&raw(serde_json::json!([
            {"name": "queued", "status": "QUEUED", "conclusion": null},
            {"name": "ok", "status": "COMPLETED", "conclusion": "SUCCESS", "detailsUrl": "https://ci/1"},
            {"name": "timeout", "status": "COMPLETED", "conclusion": "TIMED_OUT"},
            {"name": "gate", "status": "COMPLETED", "conclusion": "ACTION_REQUIRED"},
            {"context": "legacy", "state": "ERROR", "targetUrl": "https://ci/2"},
            {"context": "expected", "state": "EXPECTED"},
            {"name": "skip", "status": "COMPLETED", "conclusion": "SKIPPED"},
            {"name": "odd", "status": "COMPLETED", "conclusion": "STALE"},
        ])));
        let statuses: Vec<_> = checks.iter().map(|c| (c.name.as_str(), c.status)).collect();
        assert_eq!(
            statuses,
            [
                ("timeout", CheckStatus::Failure),
                ("legacy", CheckStatus::Failure),
                ("queued", CheckStatus::Pending),
                ("ok", CheckStatus::Success),
                ("gate", CheckStatus::ActionRequired),
                ("expected", CheckStatus::Pending),
                ("skip", CheckStatus::Skipped),
                ("odd", CheckStatus::Neutral),
            ]
        );
        assert_eq!(checks[1].url.as_deref(), Some("https://ci/2"));
    }

    #[test]
    fn keeps_a_workflow_waiting_for_approval_out_of_passing() {
        let rollup = raw(serde_json::json!([
            {"name": "ok", "status": "COMPLETED", "conclusion": "SUCCESS"},
            {"name": "gate", "status": "COMPLETED", "conclusion": "ACTION_REQUIRED"},
        ]));
        assert_eq!(checks_state(&rollup), Some(ChecksState::Pending));
    }

    #[test]
    fn reads_the_listing_rollup_state_without_names() {
        let state = |s: &str| checks_state(&raw(serde_json::json!([{ "state": s }])));
        assert_eq!(state("SUCCESS"), Some(ChecksState::Passing));
        assert_eq!(state("FAILURE"), Some(ChecksState::Failing));
        assert_eq!(state("ERROR"), Some(ChecksState::Failing));
        assert_eq!(state("PENDING"), Some(ChecksState::Pending));
        assert_eq!(checks_state(&[]), None);
    }
}
