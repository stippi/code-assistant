//! Run/delegation convergence — the type skeleton for the ROADMAP's
//! "Next: unify runs and delegation".
//!
//! This module is deliberately vocabulary only: no store, no runner trait
//! implementation, no policy. It pins the run/attempt split — a logical run
//! ([`RunRecord`]) is distinct from each claim-and-execution ([`RunAttempt`]),
//! so a retry never rewrites history — and the description shape
//! ([`RunSpec`]) that inline fork/join, background session runs, durable
//! supervised runs, and future work-graph workers are meant to share.
//! Behavior lands with the first converged consumer; fields here may still be
//! sharpened then (they are serialized nowhere yet).
//!
//! Budgets are owner policy, not a platform mandate: every envelope field is
//! optional so a host may deliberately launch unbounded runs (PAL's
//! supervised children run to completion, limited only at launch time).

use crate::OwnerKey;
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

/// What to run, for whom, and inside which envelope. The owner-facing
/// description a policy (inline, background, durable, work-graph) executes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSpec {
    /// Who the run belongs to (host-defined identity, e.g. a session or lane).
    pub owner: OwnerKey,
    /// The role the run plays for its owner (e.g. `sub_agent`, `goal_turn`).
    pub role: String,
    /// The instructions the run executes.
    pub instructions: String,
    /// Model override; `None` inherits the host default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Tool/capability profile name, resolved by the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_profile: Option<String>,
    /// Permission tier/policy name, resolved by the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<String>,
    /// Project or working directory the run is scoped to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    /// Where the run executes (local process, sandbox, remote target).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_target: Option<String>,
    /// Optional envelope; all-`None` means run to completion (owner policy).
    #[serde(default)]
    pub budget: RunBudget,
    /// What the caller expects back (shapes the final report).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_output: Option<String>,
}

/// Optional launch-time envelope for a run. Distinct from the goal `Budget`:
/// a goal bounds *attempts across turns*, a run envelope bounds one
/// delegated execution.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunBudget {
    /// Wall-clock deadline after which the run should be stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<NaiveDateTime>,
    /// Maximum delegation depth below this run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// Maximum concurrently running delegations of the same owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<u32>,
}

/// Lifecycle of a logical run. Terminal states are dead ends; `Interrupted`
/// marks a run whose process died mid-execution and is what a supervisor's
/// retry policy acts on (spawning a new [`RunAttempt`], never rewriting one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Pending,
    Running,
    Done,
    Failed,
    Cancelled,
    Interrupted,
}

impl RunStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled
        )
    }
}

/// The durable identity and ledger of one logical run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: String,
    /// Optimistic-concurrency revision, same semantics as `Goal::revision`.
    pub revision: u64,
    pub spec: RunSpec,
    /// The run this one was delegated from, when nested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    /// The goal this run works toward, when goal-driven.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
    /// The work-graph item this run claims, when graph-driven.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    pub status: RunStatus,
    /// Attempt history; the latest entry is the authoritative execution.
    #[serde(default)]
    pub attempts: Vec<RunAttempt>,
    /// Bounded final result (the summary handed back to the caller).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// One claim and execution of a run. A retry appends a new attempt; it never
/// rewrites an earlier one — the same crash-cannot-refund-work invariant the
/// goal ledger enforces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunAttempt {
    /// 1-based attempt number within the run.
    pub number: u32,
    pub started_at: NaiveDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<NaiveDateTime>,
    /// Terminal status of this attempt; `None` while executing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<RunStatus>,
    /// Bounded factual account of what the attempt did.
    #[serde(default)]
    pub summary: String,
    /// Artifacts produced (paths, references).
    #[serde(default)]
    pub artifacts: Vec<String>,
    /// Structured evidence lines (tool status, checks, resource writes).
    #[serde(default)]
    pub evidence: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 7, 16)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
    }

    #[test]
    fn a_minimal_spec_serializes_without_optional_noise() {
        let spec = RunSpec {
            owner: OwnerKey::from_parts(&["session", "s1"]),
            role: "sub_agent".into(),
            instructions: "do the thing".into(),
            model: None,
            tool_profile: None,
            permissions: None,
            workdir: None,
            execution_target: None,
            budget: RunBudget::default(),
            expected_output: None,
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "owner": "session:s1",
                "role": "sub_agent",
                "instructions": "do the thing",
                "budget": {},
            })
        );
        let roundtrip: RunSpec = serde_json::from_value(json).unwrap();
        assert_eq!(roundtrip, spec);
    }

    #[test]
    fn a_record_roundtrips_with_its_attempt_ledger() {
        let record = RunRecord {
            id: "run-1".into(),
            revision: 3,
            spec: RunSpec {
                owner: OwnerKey::from_parts(&["session", "s1"]),
                role: "goal_turn".into(),
                instructions: "pursue the goal".into(),
                model: Some("gpt-test".into()),
                tool_profile: None,
                permissions: None,
                workdir: None,
                execution_target: None,
                budget: RunBudget {
                    deadline: Some(now()),
                    max_depth: Some(1),
                    max_concurrency: None,
                },
                expected_output: None,
            },
            parent_run_id: None,
            goal_id: Some("goal-1".into()),
            work_item_id: None,
            status: RunStatus::Interrupted,
            attempts: vec![RunAttempt {
                number: 1,
                started_at: now(),
                finished_at: None,
                status: None,
                summary: "process died mid-execution".into(),
                artifacts: vec![],
                evidence: vec![],
            }],
            result: None,
            created_at: now(),
            updated_at: now(),
        };
        let json = serde_json::to_string(&record).unwrap();
        let roundtrip: RunRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip, record);
        assert!(!RunStatus::Interrupted.is_terminal(), "retryable by policy");
        assert!(RunStatus::Done.is_terminal());
    }
}
