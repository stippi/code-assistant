//! Durable goals and a work ledger: an outcome that must stay active across
//! session incarnations. An `update_plan` plan belongs to one code-assistant
//! session, and pal rotates that session daily — so a goal cannot live there.
//! A goal is the pal_core entity that survives rotation, restart and the
//! expiry watcher, carrying its own completion contract, budget, state machine
//! and an evidence ledger.
//!
//! This module is the *domain* layer only: the state machine, the budget
//! accounting and the bounded-controller policy that folds an evaluation into
//! the next decision (see [`Goal::apply_evaluation`]). Whether a turn actually
//! satisfied the contract is decided by an injected [`GoalEvaluator`] — the
//! LLM-shaped seam — so the whole policy stays testable without a model. The
//! ledger records *attempts, artifacts and evidence*, never model
//! chain-of-thought.
//!
//! All timestamps are naive local time, like `pal_core::session` and
//! `pal_core::jobs`.

use crate::OwnerKey;
use chrono::NaiveDateTime;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use uuid::Uuid;

/// Where a goal is in its lifecycle. `Done` and `Failed` are terminal.
///
/// The controller only ever *continues* an autonomous run out of `Running`.
/// Every other non-terminal state is a deliberate stop: `Waiting` on a durable
/// barrier (a later roadmap item arms these), `Blocked` on a real obstacle the
/// agent surfaced instead of looping, `Paused` by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalState {
    Running,
    Waiting,
    Blocked,
    Paused,
    Done,
    Failed,
}

impl GoalState {
    /// Terminal states admit no further transitions.
    pub fn is_terminal(&self) -> bool {
        matches!(self, GoalState::Done | GoalState::Failed)
    }

    /// A stable lowercase label for prompts and listings.
    pub fn label(&self) -> &'static str {
        match self {
            GoalState::Running => "running",
            GoalState::Waiting => "waiting",
            GoalState::Blocked => "blocked",
            GoalState::Paused => "paused",
            GoalState::Done => "done",
            GoalState::Failed => "failed",
        }
    }

    /// Whether `self -> next` is a legal edge. Re-entering the same state is
    /// always allowed (idempotent). Only `Running` may reach `Done`: a
    /// `Waiting`/`Blocked` goal must first be woken back into `Running` and
    /// take a turn before it can claim success.
    pub fn can_transition_to(&self, next: GoalState) -> bool {
        use GoalState::*;
        if *self == next {
            return !self.is_terminal();
        }
        match (self, next) {
            (Running, Waiting | Blocked | Paused | Done | Failed) => true,
            (Waiting, Running | Blocked | Paused | Failed) => true,
            (Blocked, Running | Paused | Failed) => true,
            (Paused, Running | Failed) => true,
            (Done | Failed, _) => false,
            _ => false,
        }
    }
}

/// A checklist step under the objective. Cheap structure — the real proof of
/// progress lives in the ledger, not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subgoal {
    pub description: String,
    #[serde(default)]
    pub done: bool,
}

impl Subgoal {
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            done: false,
        }
    }
}

/// What "done" means for this goal, in terms the evaluator can check against.
/// The agent may declare success only against this contract — never because a
/// turn *felt* finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionContract {
    /// The outcome in the user's terms ("a filled-in tax form at …").
    pub outcome: String,
    /// How success is verified (a command, a check, an artifact's existence).
    pub verification: String,
    /// Things that must hold throughout ("keep the original file intact").
    #[serde(default)]
    pub constraints: Vec<String>,
    /// Hard limits on what the agent may do ("never submit, only prepare").
    #[serde(default)]
    pub boundaries: Vec<String>,
    /// When to give up rather than keep trying.
    pub stop_condition: String,
}

impl CompletionContract {
    pub fn new(
        outcome: impl Into<String>,
        verification: impl Into<String>,
        stop_condition: impl Into<String>,
    ) -> Self {
        Self {
            outcome: outcome.into(),
            verification: verification.into(),
            constraints: Vec::new(),
            boundaries: Vec::new(),
            stop_condition: stop_condition.into(),
        }
    }
}

/// The resource envelope for autonomous continuation. A goal that exhausts its
/// budget fails rather than looping forever.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    /// Maximum number of attempts (turns) the controller will drive. Reaching
    /// it without success is a failure, not a silent stop.
    pub max_turns: u32,
    /// Optional wall-clock deadline; past it the goal fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<NaiveDateTime>,
}

impl Budget {
    pub fn turns(max_turns: u32) -> Self {
        Self {
            max_turns,
            deadline: None,
        }
    }

    pub fn with_deadline(mut self, deadline: NaiveDateTime) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Whether the deadline (if any) has been reached at `now`.
    pub fn deadline_passed(&self, now: NaiveDateTime) -> bool {
        self.deadline.is_some_and(|dl| now >= dl)
    }
}

/// The evaluator's judgement of one attempt against the contract. This is the
/// only channel through which a goal reaches `Done`: the injected evaluator is
/// what checks the completion contract, the controller merely trusts its
/// verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptVerdict {
    /// Real progress; keep going if budget allows.
    Progressed,
    /// The completion contract is met — the goal is done.
    Satisfied,
    /// A genuine obstacle the agent cannot clear itself (surface, don't loop).
    Blocked,
    /// The goal needs an answer from the user before it can continue.
    NeedsInput,
    /// The turn set up a durable dependency and there is nothing more to do
    /// until it resolves — the goal parks on a wait barrier instead of burning
    /// turns polling. The barrier travels in [`Evaluation::wait`]; a `Waiting`
    /// verdict with no barrier is malformed and surfaces as `Blocked`.
    Waiting,
    /// The completion contract's explicit stop condition was met; continuing
    /// would violate the user's envelope.
    Stopped,
    /// The controller could not obtain a trustworthy evaluation for the turn
    /// (runtime failure, timeout, malformed judge response). Never emitted by
    /// a [`GoalEvaluator`]; recorded so failures cannot refund budget.
    Error,
}

/// One recorded attempt: what was tried and the evidence for it, never the
/// model's reasoning. Appended to the goal's ledger by [`Goal::apply_evaluation`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub at: NaiveDateTime,
    pub verdict: AttemptVerdict,
    /// A short, factual account of what the turn did.
    pub summary: String,
    /// Artifacts (paths/refs) the turn produced or changed.
    #[serde(default)]
    pub artifacts: Vec<String>,
    /// Verification output supporting the verdict (command results, checks).
    #[serde(default)]
    pub evidence: Vec<String>,
}

/// A controller turn that has claimed budget durably but has not yet been
/// folded into the evidence ledger. Persisting this marker before dispatch is
/// what makes the turn budget survive evaluator failures and process crashes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InFlightAttempt {
    pub started_at: NaiveDateTime,
    /// Revision assigned to this claim. Unlike the timestamp, this changes
    /// when a busy claim is abandoned and the goal is reclaimed immediately.
    #[serde(default)]
    pub claim_revision: u64,
}

/// A durable goal owned by one owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    pub id: String,
    /// Optimistic-concurrency revision. Store updates only succeed against the
    /// revision that was loaded, so a controller snapshot cannot overwrite a
    /// newer user transition.
    #[serde(default)]
    pub revision: u64,
    /// The owner whose incarnations pursue this goal (and whose channel hears
    /// about it).
    #[serde(alias = "lane")]
    pub owner: OwnerKey,
    /// The user's objective, verbatim intent.
    pub objective: String,
    #[serde(default)]
    pub subgoals: Vec<Subgoal>,
    pub contract: CompletionContract,
    pub budget: Budget,
    pub state: GoalState,
    #[serde(default)]
    pub attempts: Vec<Attempt>,
    /// Present from immediately before an autonomous turn is dispatched until
    /// its evaluation (or controller error) is recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_flight: Option<InFlightAttempt>,
    /// Human-readable reason for the current `Blocked`/`Failed`/`Paused`
    /// state; cleared when the goal returns to `Running`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

impl Goal {
    pub fn new(
        id: impl Into<String>,
        owner: OwnerKey,
        objective: impl Into<String>,
        contract: CompletionContract,
        budget: Budget,
        now: NaiveDateTime,
    ) -> Self {
        Self {
            id: id.into(),
            revision: 0,
            owner,
            objective: objective.into(),
            subgoals: Vec::new(),
            contract,
            budget,
            state: GoalState::Running,
            attempts: Vec::new(),
            in_flight: None,
            note: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Attempts recorded so far — the turn budget is spent per attempt.
    pub fn turns_used(&self) -> u32 {
        self.attempts.len() as u32 + u32::from(self.in_flight.is_some())
    }

    /// Turns still available before the budget is exhausted.
    pub fn turns_remaining(&self) -> u32 {
        self.budget.max_turns.saturating_sub(self.turns_used())
    }

    /// Claim one autonomous turn from the budget. The caller persists this
    /// state before dispatching work; only then is a crash unable to erase the
    /// fact that the turn was attempted.
    pub fn begin_attempt(&mut self, now: NaiveDateTime) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.state == GoalState::Running,
            "cannot begin an attempt for a goal in state {:?}",
            self.state
        );
        anyhow::ensure!(
            self.in_flight.is_none(),
            "goal already has an in-flight attempt"
        );
        anyhow::ensure!(
            !self.budget.deadline_passed(now),
            "cannot begin an attempt after the goal deadline"
        );
        anyhow::ensure!(self.turns_remaining() > 0, "goal turn budget is exhausted");
        self.in_flight = Some(InFlightAttempt {
            started_at: now,
            claim_revision: self.revision.saturating_add(1),
        });
        self.updated_at = now;
        Ok(())
    }

    /// Close the in-flight attempt after a runtime/evaluator failure. The
    /// failed turn remains in the evidence ledger and consumes budget; retry
    /// is therefore bounded by the same envelope as successful evaluations.
    pub fn record_attempt_error(
        &mut self,
        reason: impl Into<String>,
        now: NaiveDateTime,
    ) -> anyhow::Result<ControllerDecision> {
        anyhow::ensure!(
            self.in_flight.take().is_some(),
            "cannot record an attempt error without an in-flight attempt"
        );
        let reason = reason.into();
        self.attempts.push(Attempt {
            at: now,
            verdict: AttemptVerdict::Error,
            summary: reason.clone(),
            artifacts: Vec::new(),
            evidence: Vec::new(),
        });
        self.updated_at = now;

        if self.budget.deadline_passed(now) {
            if !self.state.is_terminal() {
                self.transition(GoalState::Failed, now)?;
            }
            self.note = Some("deadline passed".into());
            Ok(ControllerDecision::Failed(FailureReason::DeadlinePassed))
        } else if self.turns_remaining() == 0 {
            if !self.state.is_terminal() {
                self.transition(GoalState::Failed, now)?;
            }
            self.note = Some(format!(
                "turn budget exhausted after controller error: {reason}"
            ));
            Ok(ControllerDecision::Failed(
                FailureReason::TurnBudgetExhausted,
            ))
        } else {
            Ok(ControllerDecision::Continue)
        }
    }

    /// Move to `next`, validating the edge and stamping `updated_at`. Errors on
    /// an illegal transition rather than silently corrupting the state machine.
    pub fn transition(&mut self, next: GoalState, now: NaiveDateTime) -> anyhow::Result<()> {
        if !self.state.can_transition_to(next) {
            anyhow::bail!("illegal goal transition {:?} -> {:?}", self.state, next);
        }
        self.state = next;
        self.updated_at = now;
        Ok(())
    }

    /// Fold one turn's [`Evaluation`] into the goal: append it to the ledger
    /// (spending a turn) and let the bounded-controller policy pick what
    /// happens next. Only a `Running` goal is evaluable; the controller reaches
    /// `Done` solely through a `Satisfied` verdict, surfaces a `Blocked` /
    /// `NeedsInput` verdict instead of looping, and fails a `Progressed` goal
    /// that has run out of budget or passed its deadline rather than continuing
    /// forever. See [`ControllerDecision`].
    pub fn apply_evaluation(
        &mut self,
        eval: Evaluation,
        now: NaiveDateTime,
    ) -> anyhow::Result<ControllerDecision> {
        if self.state != GoalState::Running {
            anyhow::bail!(
                "cannot evaluate a goal in state {:?}; only Running is evaluable",
                self.state
            );
        }
        anyhow::ensure!(
            self.in_flight.is_some(),
            "cannot evaluate a goal without an in-flight attempt"
        );
        self.in_flight = None;
        for subgoal in &mut self.subgoals {
            if eval
                .completed_subgoals
                .iter()
                .any(|completed| completed == &subgoal.description)
            {
                subgoal.done = true;
            }
        }
        // Record the attempt (spends a turn) before deciding — the reason for a
        // block/needs-input is the attempt's own summary.
        let wait_request = eval.wait;
        let reason = eval.summary.clone();
        self.attempts.push(Attempt {
            at: now,
            verdict: eval.verdict,
            summary: eval.summary,
            artifacts: eval.artifacts,
            evidence: eval.evidence,
        });

        // A wall-clock deadline is a hard envelope, including for a turn that
        // started before it but only finished afterwards. The evidence stays
        // on the ledger, but late success cannot turn the goal into Done.
        if self.budget.deadline_passed(now) {
            self.transition(GoalState::Failed, now)?;
            self.note = Some("deadline passed".into());
            return Ok(ControllerDecision::Failed(FailureReason::DeadlinePassed));
        }

        let decision = match eval.verdict {
            // The evaluator is the only path to Done — it checked the contract.
            AttemptVerdict::Satisfied => {
                self.transition(GoalState::Done, now)?;
                self.note = None;
                ControllerDecision::Done
            }
            AttemptVerdict::Blocked => {
                self.transition(GoalState::Blocked, now)?;
                self.note = Some(reason);
                ControllerDecision::Blocked
            }
            AttemptVerdict::NeedsInput => {
                self.transition(GoalState::Blocked, now)?;
                self.note = Some(reason);
                ControllerDecision::AwaitInput
            }
            AttemptVerdict::Stopped => {
                self.transition(GoalState::Failed, now)?;
                self.note = Some(reason);
                ControllerDecision::Failed(FailureReason::StopConditionMet)
            }
            // The turn armed a durable dependency: park until it resolves. A
            // Waiting verdict is only honoured with a barrier to wait on;
            // without one it is a malformed judgement, surfaced as a block
            // rather than an indefinite park on nothing.
            AttemptVerdict::Waiting => match wait_request {
                Some(request) => {
                    self.transition(GoalState::Waiting, now)?;
                    self.note = Some(reason);
                    ControllerDecision::Wait(request)
                }
                None => {
                    self.transition(GoalState::Blocked, now)?;
                    self.note = Some(format!("asked to wait but named no barrier: {reason}"));
                    ControllerDecision::Blocked
                }
            },
            AttemptVerdict::Error => {
                anyhow::bail!("controller-error verdicts cannot come from a goal evaluator")
            }
            // Progress is only allowed to continue while the envelope holds;
            // the hard deadline was already checked above.
            AttemptVerdict::Progressed => {
                if self.turns_remaining() == 0 {
                    self.transition(GoalState::Failed, now)?;
                    self.note = Some("turn budget exhausted".into());
                    ControllerDecision::Failed(FailureReason::TurnBudgetExhausted)
                } else {
                    ControllerDecision::Continue
                }
            }
        };
        Ok(decision)
    }

    /// Fail a non-terminal goal whose deadline has passed (the sweep the
    /// controller runs over `Waiting`/`Blocked`/`Paused` goals that never took
    /// another turn). Returns whether it fired.
    pub fn enforce_deadline(&mut self, now: NaiveDateTime) -> bool {
        if self.state.is_terminal() || !self.budget.deadline_passed(now) {
            return false;
        }
        // `Failed` is reachable from every non-terminal state.
        let _ = self.transition(GoalState::Failed, now);
        self.note = Some("deadline passed".into());
        true
    }

    /// User preemption / lifecycle edges. Each is a thin, note-managing wrapper
    /// over [`Goal::transition`]; an illegal edge surfaces as an error.
    pub fn pause(&mut self, now: NaiveDateTime) -> anyhow::Result<()> {
        self.transition(GoalState::Paused, now)
    }

    /// Return a paused or blocked goal to `Running`, clearing the stop reason.
    pub fn resume(&mut self, now: NaiveDateTime) -> anyhow::Result<()> {
        self.transition(GoalState::Running, now)?;
        self.note = None;
        Ok(())
    }

    /// Arm a durable wait barrier (`Running` -> `Waiting`).
    pub fn wait(&mut self, now: NaiveDateTime) -> anyhow::Result<()> {
        self.transition(GoalState::Waiting, now)
    }

    /// A wait barrier fired (`Waiting` -> `Running`), clearing any note.
    pub fn wake(&mut self, now: NaiveDateTime) -> anyhow::Result<()> {
        self.transition(GoalState::Running, now)?;
        self.note = None;
        Ok(())
    }

    /// Surface a real obstacle (`Running`/`Waiting` -> `Blocked`) with a reason.
    pub fn block(&mut self, reason: impl Into<String>, now: NaiveDateTime) -> anyhow::Result<()> {
        self.transition(GoalState::Blocked, now)?;
        self.note = Some(reason.into());
        Ok(())
    }

    /// Give up on the goal with a reason (`* ` -> `Failed`).
    pub fn fail(&mut self, reason: impl Into<String>, now: NaiveDateTime) -> anyhow::Result<()> {
        self.transition(GoalState::Failed, now)?;
        self.note = Some(reason.into());
        Ok(())
    }

    // --- Store-level folds -------------------------------------------------
    //
    // The claim/finish/abandon protocol is policy, not persistence: every
    // repository implementation (the JSON store here, a host's transactional
    // store) must apply exactly the same per-goal transition or the token
    // guarantees silently diverge. These folds mutate `self` (the currently
    // persisted goal) against a caller-held snapshot/claim and tell the store
    // whether — and what — to persist.

    /// Claim fold: begin an attempt against `snapshot`, or report why not.
    /// Mutates `self` (revision bumped) except in the `Stale` case.
    pub fn fold_claim(&mut self, snapshot: &Goal, now: NaiveDateTime) -> anyhow::Result<ClaimFold> {
        if self.revision != snapshot.revision
            || self.state != GoalState::Running
            || self.in_flight.is_some()
        {
            return Ok(ClaimFold::Stale);
        }
        if self.enforce_deadline(now) {
            self.revision = self.revision.saturating_add(1);
            return Ok(ClaimFold::Enforced);
        }
        if self.turns_remaining() == 0 {
            self.fail("turn budget exhausted", now)?;
            self.revision = self.revision.saturating_add(1);
            return Ok(ClaimFold::Enforced);
        }

        self.begin_attempt(now)?;
        self.revision = self.revision.saturating_add(1);
        Ok(ClaimFold::Claimed)
    }

    /// Finish fold: close the claimed attempt on `self`, merging a concurrent
    /// user transition instead of overwriting it (see
    /// [`GoalStore::finish_attempt`] for the semantics). `None` when the claim
    /// token no longer matches — `self` is untouched and must not be
    /// persisted. Errors if `claim` carries no in-flight token.
    pub fn fold_finish(
        &mut self,
        claim: &Goal,
        completion: AttemptCompletion,
        now: NaiveDateTime,
    ) -> anyhow::Result<Option<ControllerDecision>> {
        let Some(token) = claim.in_flight.as_ref() else {
            anyhow::bail!("cannot finish a goal snapshot without an in-flight attempt");
        };
        if self.in_flight.as_ref() != Some(token) {
            return Ok(None);
        }

        let stopped_state =
            (self.state != GoalState::Running).then_some((self.state, self.note.clone()));
        let was_terminal = self.state.is_terminal();
        let decision = match completion {
            AttemptCompletion::Evaluated(evaluation) => {
                // The attempt itself was claimed while Running. Temporarily
                // restore that state to fold its verdict, then reinstate a
                // concurrent user stop unless the attempt reached a terminal
                // outcome. Thus progress never resumes a preempted goal, while
                // verified completion and hard envelope failures remain final.
                if stopped_state.is_some() {
                    self.state = GoalState::Running;
                }
                let decision = self.apply_evaluation(evaluation, now)?;
                if let Some((state, note)) = stopped_state {
                    if was_terminal || !self.state.is_terminal() {
                        self.state = state;
                        self.note = note;
                        self.updated_at = now;
                        ControllerDecision::Preempted
                    } else {
                        decision
                    }
                } else {
                    decision
                }
            }
            AttemptCompletion::ControllerError(reason) => {
                let decision = self.record_attempt_error(reason, now)?;
                if let Some((state, note)) = stopped_state {
                    if was_terminal {
                        self.state = state;
                        self.note = note;
                        self.updated_at = now;
                        ControllerDecision::Preempted
                    } else {
                        decision
                    }
                } else {
                    decision
                }
            }
        };
        self.revision = self.revision.saturating_add(1);
        Ok(Some(decision))
    }

    /// Abandon fold: release the claim without spending budget. `false` when
    /// the token no longer matches (`self` untouched). Errors if `claim`
    /// carries no in-flight token.
    pub fn fold_abandon(&mut self, claim: &Goal) -> anyhow::Result<bool> {
        let Some(token) = claim.in_flight.as_ref() else {
            anyhow::bail!("cannot abandon a goal snapshot without an in-flight attempt");
        };
        if self.in_flight.as_ref() != Some(token) {
            return Ok(false);
        }
        self.in_flight = None;
        self.revision = self.revision.saturating_add(1);
        Ok(true)
    }
}

/// Outcome of [`Goal::fold_claim`]: what the repository must do with the
/// mutated goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimFold {
    /// The snapshot is stale or the goal is not claimable — nothing changed,
    /// nothing to persist, no work may be dispatched.
    Stale,
    /// The envelope fired (deadline passed / budget exhausted): the goal
    /// mutated and must be persisted, but no attempt was claimed.
    Enforced,
    /// An attempt was begun: persist the goal, then dispatch the turn.
    Claimed,
}

/// One turn's judgement against the contract, produced by a [`GoalEvaluator`]
/// and folded into the goal by [`Goal::apply_evaluation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub verdict: AttemptVerdict,
    /// A short, factual account of what the turn did (becomes the attempt
    /// summary, and the block/needs-input note where relevant).
    pub summary: String,
    pub artifacts: Vec<String>,
    pub evidence: Vec<String>,
    /// Checklist entries for which this turn supplied concrete evidence.
    pub completed_subgoals: Vec<String>,
    /// The durable barrier to park on, set only with an
    /// [`AttemptVerdict::Waiting`] verdict. The controller arms it and moves the
    /// goal to `Waiting`; a `Waiting` verdict without one is treated as a block.
    pub wait: Option<crate::waits::WaitRequest>,
}

impl Evaluation {
    /// Convenience for a verdict with just a summary and no artifacts/evidence.
    pub fn new(verdict: AttemptVerdict, summary: impl Into<String>) -> Self {
        Self {
            verdict,
            summary: summary.into(),
            artifacts: Vec::new(),
            evidence: Vec::new(),
            completed_subgoals: Vec::new(),
            wait: None,
        }
    }

    /// A `Waiting` evaluation that parks the goal on `request` until it fires.
    pub fn waiting(summary: impl Into<String>, request: crate::waits::WaitRequest) -> Self {
        Self {
            wait: Some(request),
            ..Self::new(AttemptVerdict::Waiting, summary)
        }
    }
}

/// Why a goal failed. Distinct from a `Blocked` obstacle: a failure is the
/// controller giving up because the envelope is spent, not a solvable block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureReason {
    /// `max_turns` attempts were spent without satisfying the contract.
    TurnBudgetExhausted,
    /// The wall-clock deadline passed.
    DeadlinePassed,
    /// The completion contract's explicit stop condition was met.
    StopConditionMet,
}

/// What the controller decided after folding an evaluation. The runtime acts on
/// this: run another turn, stop, arm a wait, or tell the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerDecision {
    /// Budget remains and the contract is not yet met — take another turn.
    Continue,
    /// The contract is satisfied; the goal is `Done`.
    Done,
    /// A real obstacle was surfaced; the goal is `Blocked` awaiting resolution.
    Blocked,
    /// The goal needs the user to answer before it can continue.
    AwaitInput,
    /// The goal parked on a durable barrier; the runtime arms it (see
    /// [`crate::waits`]) and the goal spends no turns until it resolves.
    Wait(crate::waits::WaitRequest),
    /// The goal failed; the envelope gave out.
    Failed(FailureReason),
    /// A user or another controller changed the goal while the claimed turn
    /// was running. Its ledger entry was recorded, but the newer stopped state
    /// remains authoritative.
    Preempted,
}

/// How a claimed controller turn ended. Folded atomically by
/// [`GoalStore::finish_attempt`] so a concurrent user transition cannot be
/// overwritten while the turn's ledger entry is still retained.
pub enum AttemptCompletion {
    Evaluated(Evaluation),
    ControllerError(String),
}

/// What the runtime observed at the end of a goal turn, handed to the
/// evaluator. Deliberately minimal and agent-stack-agnostic so pal_core stays
/// decoupled from code-assistant internals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnOutcome {
    /// The assistant's own account of what it did this turn.
    pub assistant_summary: String,
    /// Artifacts (paths/refs) the turn produced or changed.
    pub artifacts: Vec<String>,
    /// Verification output the turn gathered (command results, checks).
    pub verification: Vec<String>,
}

/// The LLM-shaped seam: judge a completed turn against the goal's contract.
/// Implementations range from a deterministic check (an artifact exists, a
/// command exit code) to a bounded model call — the controller policy is
/// oblivious to which, which is what keeps it testable without a model.
#[async_trait::async_trait]
pub trait GoalEvaluator: Send + Sync {
    async fn evaluate(&self, goal: &Goal, turn: &TurnOutcome) -> anyhow::Result<Evaluation>;
}

/// JSON-file persistence for goals (`goals.json`). Every operation reloads the
/// current file and writes through atomic tmp+rename. Instances for the same
/// path share a process-local mutex and an OS advisory lock serializes
/// read-modify-write transactions across processes. Revisions reject stale
/// snapshots and attempt tokens merge concurrent lifecycle changes.
pub struct GoalStore {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl GoalStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let lock = goal_store_lock(&path);
        Self { path, lock }
    }

    fn transaction(&self) -> anyhow::Result<(MutexGuard<'_, ()>, File)> {
        let process_guard = self.lock.lock().expect("goal store lock poisoned");
        let file_guard = lock_store_file(&self.path)?;
        Ok((process_guard, file_guard))
    }

    /// All goals, terminal ones included; a missing file yields an empty list.
    pub fn list(&self) -> anyhow::Result<Vec<Goal>> {
        let (_process_guard, _file_guard) = self.transaction()?;
        self.load_unlocked()
    }

    fn load_unlocked(&self) -> anyhow::Result<Vec<Goal>> {
        match std::fs::read_to_string(&self.path) {
            Ok(content) => Ok(serde_json::from_str(&content)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    fn save_unlocked(&self, goals: &[Goal]) -> anyhow::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(goals)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Add a goal with a caller-supplied id; the id must be unique.
    pub fn add(&self, goal: Goal) -> anyhow::Result<()> {
        let (_process_guard, _file_guard) = self.transaction()?;
        let mut goals = self.load_unlocked()?;
        if goals.iter().any(|g| g.id == goal.id) {
            anyhow::bail!("goal id {} already exists", goal.id);
        }
        goals.push(goal);
        self.save_unlocked(&goals)
    }

    /// Create and persist a goal with a store-assigned id, returned for display
    /// and control.
    pub fn add_new(
        &self,
        owner: OwnerKey,
        objective: impl Into<String>,
        contract: CompletionContract,
        budget: Budget,
        now: NaiveDateTime,
    ) -> anyhow::Result<Goal> {
        let (_process_guard, _file_guard) = self.transaction()?;
        let mut goals = self.load_unlocked()?;
        let base = format!("goal-{}", now.format("%Y%m%d-%H%M%S"));
        let mut id = base.clone();
        let mut n = 1;
        while goals.iter().any(|g| g.id == id) {
            n += 1;
            id = format!("{base}-{n}");
        }
        let goal = Goal::new(id, owner, objective, contract, budget, now);
        goals.push(goal.clone());
        self.save_unlocked(&goals)?;
        Ok(goal)
    }

    /// Replace every non-terminal goal owned by `owner` with one fresh goal,
    /// in a single read-modify-write transaction. Terminal evidence ledgers
    /// remain history. This is the persistence primitive for hosts that expose
    /// exactly one current goal per owner. Returns the new goal and the number
    /// of current goals it replaced.
    pub fn replace_current_for_owner(
        &self,
        owner: OwnerKey,
        objective: impl Into<String>,
        contract: CompletionContract,
        budget: Budget,
        now: NaiveDateTime,
    ) -> anyhow::Result<(Goal, usize)> {
        let (_process_guard, _file_guard) = self.transaction()?;
        let mut goals = self.load_unlocked()?;
        let before = goals.len();
        goals.retain(|goal| goal.owner != owner || goal.state.is_terminal());
        let replaced = before - goals.len();
        // Replacement deletes the old ledger entry, but durable waits and
        // child links can outlive it briefly. A UUID prevents the new goal
        // from ever addressing those links, including after cancel + set in
        // the same second.
        let id = format!(
            "goal-{}-{}",
            now.format("%Y%m%d-%H%M%S"),
            Uuid::new_v4().simple()
        );
        let goal = Goal::new(id, owner, objective, contract, budget, now);
        goals.push(goal.clone());
        self.save_unlocked(&goals)?;
        Ok((goal, replaced))
    }

    /// Remove every non-terminal goal owned by `owner` in one transaction,
    /// preserving terminal evidence ledgers. Returns the number removed.
    pub fn remove_current_for_owner(&self, owner: &OwnerKey) -> anyhow::Result<usize> {
        let (_process_guard, _file_guard) = self.transaction()?;
        let mut goals = self.load_unlocked()?;
        let before = goals.len();
        goals.retain(|goal| &goal.owner != owner || goal.state.is_terminal());
        let removed = before - goals.len();
        if removed > 0 {
            self.save_unlocked(&goals)?;
        }
        Ok(removed)
    }

    /// A single goal by id.
    pub fn get(&self, id: &str) -> anyhow::Result<Option<Goal>> {
        Ok(self.list()?.into_iter().find(|g| g.id == id))
    }

    /// Persist a mutated goal (after `apply_evaluation`, a lifecycle edge, …).
    /// Errors if the id is unknown — an update never silently creates.
    pub fn update(&self, goal: &Goal) -> anyhow::Result<Goal> {
        let (_process_guard, _file_guard) = self.transaction()?;
        let mut goals = self.load_unlocked()?;
        let Some(slot) = goals.iter_mut().find(|g| g.id == goal.id) else {
            anyhow::bail!("unknown goal id {}", goal.id);
        };
        anyhow::ensure!(
            slot.revision == goal.revision,
            "goal {} revision conflict: expected {}, found {}",
            goal.id,
            goal.revision,
            slot.revision
        );
        let mut persisted = goal.clone();
        persisted.revision = persisted.revision.saturating_add(1);
        *slot = persisted.clone();
        self.save_unlocked(&goals)?;
        Ok(persisted)
    }

    /// Atomically claim one controller turn against a previously loaded
    /// snapshot. `None` means the snapshot became stale or the goal is no
    /// longer runnable; callers must not dispatch work in that case.
    pub fn claim_attempt(
        &self,
        snapshot: &Goal,
        now: NaiveDateTime,
    ) -> anyhow::Result<Option<Goal>> {
        let (_process_guard, _file_guard) = self.transaction()?;
        let mut goals = self.load_unlocked()?;
        let Some(goal) = goals.iter_mut().find(|goal| goal.id == snapshot.id) else {
            return Ok(None);
        };
        match goal.fold_claim(snapshot, now)? {
            ClaimFold::Stale => Ok(None),
            ClaimFold::Enforced => {
                self.save_unlocked(&goals)?;
                Ok(None)
            }
            ClaimFold::Claimed => {
                let claimed = goal.clone();
                self.save_unlocked(&goals)?;
                Ok(Some(claimed))
            }
        }
    }

    /// Atomically close a previously claimed attempt. The in-flight marker is
    /// the claim token: newer user transitions may advance the goal revision,
    /// but as long as they preserve that marker the completed turn is merged
    /// into the ledger without overwriting their state.
    pub fn finish_attempt(
        &self,
        claim: &Goal,
        completion: AttemptCompletion,
        now: NaiveDateTime,
    ) -> anyhow::Result<Option<(Goal, ControllerDecision)>> {
        anyhow::ensure!(
            claim.in_flight.is_some(),
            "cannot finish a goal snapshot without an in-flight attempt"
        );
        let (_process_guard, _file_guard) = self.transaction()?;
        let mut goals = self.load_unlocked()?;
        let Some(goal) = goals.iter_mut().find(|goal| goal.id == claim.id) else {
            return Ok(None);
        };
        let Some(decision) = goal.fold_finish(claim, completion, now)? else {
            return Ok(None);
        };
        let finished = goal.clone();
        self.save_unlocked(&goals)?;
        Ok(Some((finished, decision)))
    }

    /// Release a claim when the backend atomically reports that another turn
    /// already owns the session. No goal work was dispatched, so this is the
    /// sole path that clears an in-flight marker without spending budget.
    pub fn abandon_attempt(&self, claim: &Goal) -> anyhow::Result<Option<Goal>> {
        anyhow::ensure!(
            claim.in_flight.is_some(),
            "cannot abandon a goal snapshot without an in-flight attempt"
        );
        let (_process_guard, _file_guard) = self.transaction()?;
        let mut goals = self.load_unlocked()?;
        let Some(goal) = goals.iter_mut().find(|goal| goal.id == claim.id) else {
            return Ok(None);
        };
        if !goal.fold_abandon(claim)? {
            return Ok(None);
        }
        let abandoned = goal.clone();
        self.save_unlocked(&goals)?;
        Ok(Some(abandoned))
    }

    /// Remove a goal; `false` when the id is unknown.
    pub fn remove(&self, id: &str) -> anyhow::Result<bool> {
        let (_process_guard, _file_guard) = self.transaction()?;
        let mut goals = self.load_unlocked()?;
        let before = goals.len();
        goals.retain(|g| g.id != id);
        let removed = goals.len() != before;
        if removed {
            self.save_unlocked(&goals)?;
        }
        Ok(removed)
    }

    /// Non-terminal goals — the ones a controller sweep must still drive.
    pub fn active(&self) -> anyhow::Result<Vec<Goal>> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|g| !g.state.is_terminal())
            .collect())
    }

    /// Non-terminal goals owned by one owner (e.g. to preempt on a user message).
    pub fn active_for_owner(&self, owner: &OwnerKey) -> anyhow::Result<Vec<Goal>> {
        Ok(self
            .active()?
            .into_iter()
            .filter(|g| &g.owner == owner)
            .collect())
    }

    /// Pause every goal the controller is actively driving on `owner` because
    /// a user message just arrived — the human is taking the wheel, and the
    /// autonomous loop must not race their turn on the same incarnation. Only
    /// `Running` goals are affected: a `Waiting`/`Blocked` goal is already
    /// stopped, and a `Paused` one stays paused. Returns the ids paused (empty
    /// when there was nothing to preempt). A paused goal is resumed
    /// deliberately, through the host's goal commands.
    pub fn preempt_owner(
        &self,
        owner: &OwnerKey,
        now: NaiveDateTime,
    ) -> anyhow::Result<Vec<String>> {
        let (_process_guard, _file_guard) = self.transaction()?;
        let mut goals = self.load_unlocked()?;
        let mut paused = Vec::new();
        for goal in goals
            .iter_mut()
            .filter(|goal| &goal.owner == owner && !goal.state.is_terminal())
        {
            if goal.state == GoalState::Running && goal.pause(now).is_ok() {
                goal.revision = goal.revision.saturating_add(1);
                paused.push(goal.id.clone());
            }
        }
        if !paused.is_empty() {
            self.save_unlocked(&goals)?;
        }
        Ok(paused)
    }

    /// Wake a goal a durable wait barrier just resolved: `Waiting -> Running`,
    /// stamping `note` as the wake reason (why it is running again). Atomic and
    /// idempotent-safe: `false` when the goal is gone or no longer `Waiting` (a
    /// user resumed, paused or cancelled it meanwhile, or it already woke) — the
    /// caller then simply drops the now-stale wait. This is the *only* edge from
    /// `Waiting` back to `Running` the runtime takes, so waking is unambiguous.
    pub fn wake_waiting(
        &self,
        goal_id: &str,
        note: Option<String>,
        now: NaiveDateTime,
    ) -> anyhow::Result<bool> {
        let (_process_guard, _file_guard) = self.transaction()?;
        let mut goals = self.load_unlocked()?;
        let Some(goal) = goals.iter_mut().find(|g| g.id == goal_id) else {
            return Ok(false);
        };
        if goal.state != GoalState::Waiting {
            return Ok(false);
        }
        goal.wake(now)?;
        goal.note = note;
        goal.revision = goal.revision.saturating_add(1);
        self.save_unlocked(&goals)?;
        Ok(true)
    }
}

fn goal_store_lock(path: &std::path::Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks.lock().expect("goal store lock registry poisoned");
    locks
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn lock_store_file(path: &Path) -> anyhow::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock_path = path.with_extension("json.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)?;
    FileExt::lock_exclusive(&file)?;
    Ok(file)
}

/// Storage seam for goals. The bundled JSON [`GoalStore`] implements it; a
/// host's transactional repository (see PAL's "one orchestration store" plan)
/// implements the same trait so controllers and tools never bind to a
/// concrete store. Object-safe; every method mirrors the store's documented
/// atomicity semantics (claims and finishes are revision/token-guarded).
pub trait GoalRepository: Send + Sync {
    fn list(&self) -> anyhow::Result<Vec<Goal>>;
    fn add(&self, goal: Goal) -> anyhow::Result<()>;
    /// Create and persist a goal with a store-assigned id.
    fn add_new(
        &self,
        owner: OwnerKey,
        objective: String,
        contract: CompletionContract,
        budget: Budget,
        now: NaiveDateTime,
    ) -> anyhow::Result<Goal>;
    /// Replace every non-terminal goal of `owner` with one fresh goal, in one
    /// transaction; returns the new goal and how many it replaced.
    fn replace_current_for_owner(
        &self,
        owner: OwnerKey,
        objective: String,
        contract: CompletionContract,
        budget: Budget,
        now: NaiveDateTime,
    ) -> anyhow::Result<(Goal, usize)>;
    /// Remove every non-terminal goal of `owner`; returns how many.
    fn remove_current_for_owner(&self, owner: &OwnerKey) -> anyhow::Result<usize>;
    fn get(&self, id: &str) -> anyhow::Result<Option<Goal>>;
    fn update(&self, goal: &Goal) -> anyhow::Result<Goal>;
    fn claim_attempt(&self, snapshot: &Goal, now: NaiveDateTime) -> anyhow::Result<Option<Goal>>;
    fn finish_attempt(
        &self,
        claim: &Goal,
        completion: AttemptCompletion,
        now: NaiveDateTime,
    ) -> anyhow::Result<Option<(Goal, ControllerDecision)>>;
    fn abandon_attempt(&self, claim: &Goal) -> anyhow::Result<Option<Goal>>;
    fn remove(&self, id: &str) -> anyhow::Result<bool>;
    fn active(&self) -> anyhow::Result<Vec<Goal>>;
    fn active_for_owner(&self, owner: &OwnerKey) -> anyhow::Result<Vec<Goal>>;
    fn preempt_owner(&self, owner: &OwnerKey, now: NaiveDateTime) -> anyhow::Result<Vec<String>>;
    fn wake_waiting(
        &self,
        goal_id: &str,
        note: Option<String>,
        now: NaiveDateTime,
    ) -> anyhow::Result<bool>;
}

impl GoalRepository for GoalStore {
    fn list(&self) -> anyhow::Result<Vec<Goal>> {
        GoalStore::list(self)
    }
    fn add(&self, goal: Goal) -> anyhow::Result<()> {
        GoalStore::add(self, goal)
    }
    fn add_new(
        &self,
        owner: OwnerKey,
        objective: String,
        contract: CompletionContract,
        budget: Budget,
        now: NaiveDateTime,
    ) -> anyhow::Result<Goal> {
        GoalStore::add_new(self, owner, objective, contract, budget, now)
    }
    fn replace_current_for_owner(
        &self,
        owner: OwnerKey,
        objective: String,
        contract: CompletionContract,
        budget: Budget,
        now: NaiveDateTime,
    ) -> anyhow::Result<(Goal, usize)> {
        GoalStore::replace_current_for_owner(self, owner, objective, contract, budget, now)
    }
    fn remove_current_for_owner(&self, owner: &OwnerKey) -> anyhow::Result<usize> {
        GoalStore::remove_current_for_owner(self, owner)
    }
    fn get(&self, id: &str) -> anyhow::Result<Option<Goal>> {
        GoalStore::get(self, id)
    }
    fn update(&self, goal: &Goal) -> anyhow::Result<Goal> {
        GoalStore::update(self, goal)
    }
    fn claim_attempt(&self, snapshot: &Goal, now: NaiveDateTime) -> anyhow::Result<Option<Goal>> {
        GoalStore::claim_attempt(self, snapshot, now)
    }
    fn finish_attempt(
        &self,
        claim: &Goal,
        completion: AttemptCompletion,
        now: NaiveDateTime,
    ) -> anyhow::Result<Option<(Goal, ControllerDecision)>> {
        GoalStore::finish_attempt(self, claim, completion, now)
    }
    fn abandon_attempt(&self, claim: &Goal) -> anyhow::Result<Option<Goal>> {
        GoalStore::abandon_attempt(self, claim)
    }
    fn remove(&self, id: &str) -> anyhow::Result<bool> {
        GoalStore::remove(self, id)
    }
    fn active(&self) -> anyhow::Result<Vec<Goal>> {
        GoalStore::active(self)
    }
    fn active_for_owner(&self, owner: &OwnerKey) -> anyhow::Result<Vec<Goal>> {
        GoalStore::active_for_owner(self, owner)
    }
    fn preempt_owner(&self, owner: &OwnerKey, now: NaiveDateTime) -> anyhow::Result<Vec<String>> {
        GoalStore::preempt_owner(self, owner, now)
    }
    fn wake_waiting(
        &self,
        goal_id: &str,
        note: Option<String>,
        now: NaiveDateTime,
    ) -> anyhow::Result<bool> {
        GoalStore::wake_waiting(self, goal_id, note, now)
    }
}

/// Prefix framing a goal turn's injected message (mirroring the host's
/// scheduled-job framing, e.g. PAL's `[scheduled]`). A goal turn is
/// autonomous, not a user message; the framing tells the session so.
pub const GOAL_PREFIX: &str = "[goal]";

/// The message a goal turn injects into the owner's incarnation. Frames the
/// autonomous turn against the completion contract so the agent works toward
/// the *contracted* outcome and reports what an evaluator can check — never
/// declaring success on a hunch. The evaluator judges the reply, so the turn
/// is asked for a factual progress report (no silence token, unlike a
/// scheduled job).
pub fn goal_turn_text(goal: &Goal) -> String {
    let contract = &goal.contract;
    let mut sections = vec![format!("{GOAL_PREFIX} {}", goal.objective)];

    let mut contract_lines = vec![
        format!("- Done when: {}", contract.outcome),
        format!("- Verify by: {}", contract.verification),
        format!("- Stop if: {}", contract.stop_condition),
    ];
    for c in &contract.constraints {
        contract_lines.push(format!("- Constraint (hold throughout): {c}"));
    }
    for b in &contract.boundaries {
        contract_lines.push(format!("- Boundary (never cross): {b}"));
    }
    sections.push(format!(
        "Completion contract:\n{}",
        contract_lines.join("\n")
    ));

    if !goal.subgoals.is_empty() {
        let list = goal
            .subgoals
            .iter()
            .map(|s| format!("- [{}] {}", if s.done { "x" } else { " " }, s.description))
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!("Checklist:\n{list}"));
    }

    let progress = match goal.attempts.last() {
        Some(last) => format!(
            "Progress: {} of {} attempts used. Last attempt: {}",
            goal.turns_used(),
            goal.budget.max_turns,
            last.summary
        ),
        None => format!(
            "Progress: this is the first of {} attempts allowed.",
            goal.budget.max_turns
        ),
    };
    sections.push(progress);

    // A Running goal carries a note only just after a durable wait woke it —
    // the barrier that resolved (a build finished, the time arrived, the user
    // replied) or a timeout. Surface it so the resumed turn knows what changed.
    if let Some(note) = &goal.note {
        sections.push(format!("Update since you last worked on this: {note}"));
    }

    sections.push(
        "This turn was started autonomously to advance the goal above, not by \
the user. Make concrete progress now, using your tools. Then report, in your \
reply: what you did this turn, any artifacts you produced or changed (their \
paths), and the verification output that shows where the goal stands. Only \
treat the goal as done when the contract's verification actually passes — \
never on a hunch. If you hit a real obstacle you cannot clear yourself, or you \
need a decision from the user, say so plainly instead of guessing."
            .to_string(),
    );

    sections.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    /// PAL's existing goals.json predates the OwnerKey generalization and
    /// keys the owner as "lane" — the serde alias must keep those files
    /// readable.
    #[test]
    fn legacy_lane_keyed_json_still_deserializes() {
        let goal = Goal::new(
            "g-legacy",
            owner(),
            "objective",
            CompletionContract::new("outcome", "verify", "stop"),
            Budget::turns(3),
            at(2026, 7, 15, 8, 0),
        );
        let json = serde_json::to_string(&goal)
            .unwrap()
            .replace("\"owner\"", "\"lane\"");
        let restored: Goal = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.owner, goal.owner);
    }

    fn owner() -> OwnerKey {
        OwnerKey::from_parts(&["telegram", "private", "42"])
    }

    fn goal() -> Goal {
        Goal::new(
            "goal-1",
            owner(),
            "prepare the 2025 tax return",
            CompletionContract::new(
                "a filled ELSTER draft ready to review",
                "the draft file exists and validates",
                "give up if a required document is missing",
            ),
            Budget::turns(5),
            at(2026, 7, 14, 9, 0),
        )
    }

    fn apply_at(
        goal: &mut Goal,
        evaluation: Evaluation,
        now: NaiveDateTime,
    ) -> anyhow::Result<ControllerDecision> {
        goal.begin_attempt(now)?;
        goal.apply_evaluation(evaluation, now)
    }

    #[test]
    fn new_goal_starts_running_with_a_clean_ledger() {
        let g = goal();
        assert_eq!(g.state, GoalState::Running);
        assert!(g.attempts.is_empty());
        assert_eq!(g.turns_used(), 0);
        assert_eq!(g.turns_remaining(), 5);
        assert_eq!(g.created_at, g.updated_at);
    }

    #[test]
    fn terminal_states_are_terminal() {
        assert!(GoalState::Done.is_terminal());
        assert!(GoalState::Failed.is_terminal());
        assert!(!GoalState::Running.is_terminal());
        assert!(!GoalState::Waiting.is_terminal());
        assert!(!GoalState::Blocked.is_terminal());
        assert!(!GoalState::Paused.is_terminal());
    }

    #[test]
    fn only_running_can_reach_done() {
        assert!(GoalState::Running.can_transition_to(GoalState::Done));
        assert!(!GoalState::Waiting.can_transition_to(GoalState::Done));
        assert!(!GoalState::Blocked.can_transition_to(GoalState::Done));
        assert!(!GoalState::Paused.can_transition_to(GoalState::Done));
    }

    #[test]
    fn legal_and_illegal_edges() {
        use GoalState::*;
        // Running fans out to every stop and both terminals.
        for next in [Waiting, Blocked, Paused, Done, Failed] {
            assert!(Running.can_transition_to(next), "Running -> {next:?}");
        }
        // Waiting/Blocked wake back into Running, may fail, may pause.
        assert!(Waiting.can_transition_to(Running));
        assert!(Blocked.can_transition_to(Running));
        assert!(Paused.can_transition_to(Running));
        // Terminals are dead ends, even to themselves.
        for next in [Running, Waiting, Blocked, Paused, Done, Failed] {
            assert!(!Done.can_transition_to(next), "Done -> {next:?}");
            assert!(!Failed.can_transition_to(next), "Failed -> {next:?}");
        }
        // A non-terminal state re-entering itself is a no-op edge.
        assert!(Running.can_transition_to(Running));
        // Blocked cannot jump straight to Waiting.
        assert!(!Blocked.can_transition_to(Waiting));
    }

    #[test]
    fn transition_validates_and_stamps_updated_at() {
        let mut g = goal();
        let t1 = at(2026, 7, 14, 10, 0);
        g.transition(GoalState::Waiting, t1).unwrap();
        assert_eq!(g.state, GoalState::Waiting);
        assert_eq!(g.updated_at, t1);
        assert_eq!(g.created_at, at(2026, 7, 14, 9, 0));

        // Illegal edge is rejected and leaves the state untouched.
        let err = g.transition(GoalState::Done, at(2026, 7, 14, 11, 0));
        assert!(err.is_err());
        assert_eq!(g.state, GoalState::Waiting);
        assert_eq!(g.updated_at, t1);
    }

    #[test]
    fn budget_deadline_accounting() {
        let b = Budget::turns(3).with_deadline(at(2026, 7, 14, 18, 0));
        assert!(!b.deadline_passed(at(2026, 7, 14, 17, 59)));
        assert!(b.deadline_passed(at(2026, 7, 14, 18, 0)));
        assert!(b.deadline_passed(at(2026, 7, 14, 18, 1)));
        // No deadline never passes.
        assert!(!Budget::turns(3).deadline_passed(at(2999, 1, 1, 0, 0)));
    }

    #[test]
    fn contract_defaults_have_no_constraints_or_boundaries() {
        let c = CompletionContract::new("out", "verify", "stop");
        assert!(c.constraints.is_empty());
        assert!(c.boundaries.is_empty());
    }

    // ---- bounded controller policy ------------------------------------

    #[test]
    fn progressed_within_budget_continues_and_records_the_attempt() {
        let mut g = goal(); // budget 5 turns
        let now = at(2026, 7, 14, 10, 0);
        let mut eval = Evaluation::new(AttemptVerdict::Progressed, "downloaded the receipts");
        eval.artifacts = vec!["inbox/receipts.pdf".into()];
        eval.evidence = vec!["3 files fetched".into()];

        g.begin_attempt(now).unwrap();
        let decision = g.apply_evaluation(eval, now).unwrap();
        assert_eq!(decision, ControllerDecision::Continue);
        assert_eq!(g.state, GoalState::Running);
        assert_eq!(g.turns_used(), 1);
        assert_eq!(g.turns_remaining(), 4);
        let a = g.attempts.last().unwrap();
        assert_eq!(a.at, now);
        assert_eq!(a.verdict, AttemptVerdict::Progressed);
        assert_eq!(a.artifacts, vec!["inbox/receipts.pdf".to_string()]);
        assert_eq!(a.evidence, vec!["3 files fetched".to_string()]);
        assert!(g.in_flight.is_none());
        assert_eq!(g.note, None);
    }

    #[test]
    fn evaluated_subgoal_progress_updates_the_durable_checklist() {
        let mut g = goal();
        g.subgoals = vec![
            Subgoal::new("collect receipts"),
            Subgoal::new("validate draft"),
        ];
        let mut evaluation = Evaluation::new(AttemptVerdict::Progressed, "receipts collected");
        evaluation.completed_subgoals = vec!["collect receipts".into(), "unknown".into()];

        apply_at(&mut g, evaluation, at(2026, 7, 14, 10, 0)).unwrap();

        assert!(g.subgoals[0].done);
        assert!(!g.subgoals[1].done);
    }

    #[test]
    fn an_attempt_spends_budget_before_the_turn_is_evaluated() {
        let mut g = goal();
        let started_at = at(2026, 7, 14, 9, 30);

        g.begin_attempt(started_at).unwrap();

        assert_eq!(g.turns_used(), 1);
        assert_eq!(g.turns_remaining(), 4);
        assert_eq!(g.in_flight.as_ref().unwrap().started_at, started_at);
        assert!(g.attempts.is_empty());
    }

    #[test]
    fn a_controller_error_is_ledgered_and_cannot_refund_the_turn() {
        let mut g = goal();
        g.begin_attempt(at(2026, 7, 14, 9, 30)).unwrap();

        let decision = g
            .record_attempt_error("goal evaluation timed out", at(2026, 7, 14, 9, 31))
            .unwrap();

        assert_eq!(decision, ControllerDecision::Continue);
        assert_eq!(g.turns_used(), 1);
        assert!(g.in_flight.is_none());
        assert_eq!(g.attempts[0].verdict, AttemptVerdict::Error);
        assert_eq!(g.attempts[0].summary, "goal evaluation timed out");
    }

    #[test]
    fn a_controller_error_on_the_last_turn_exhausts_the_goal() {
        let mut g = Goal::new(
            "g",
            owner(),
            "obj",
            CompletionContract::new("out", "verify", "stop"),
            Budget::turns(1),
            at(2026, 7, 14, 9, 0),
        );
        g.begin_attempt(at(2026, 7, 14, 9, 1)).unwrap();

        let decision = g
            .record_attempt_error("judge unavailable", at(2026, 7, 14, 9, 2))
            .unwrap();

        assert_eq!(
            decision,
            ControllerDecision::Failed(FailureReason::TurnBudgetExhausted)
        );
        assert_eq!(g.state, GoalState::Failed);
        assert_eq!(g.turns_used(), 1);
    }

    #[test]
    fn satisfied_marks_done_and_clears_any_note() {
        let mut g = goal();
        g.note = Some("stale".into());
        let decision = apply_at(
            &mut g,
            Evaluation::new(AttemptVerdict::Satisfied, "draft ready and validated"),
            at(2026, 7, 14, 11, 0),
        )
        .unwrap();
        assert_eq!(decision, ControllerDecision::Done);
        assert_eq!(g.state, GoalState::Done);
        assert_eq!(g.note, None);
        assert_eq!(g.turns_used(), 1);
    }

    #[test]
    fn satisfied_on_the_last_turn_beats_budget_exhaustion() {
        let mut g = Goal::new(
            "g",
            owner(),
            "obj",
            CompletionContract::new("out", "verify", "stop"),
            Budget::turns(1),
            at(2026, 7, 14, 9, 0),
        );
        let decision = apply_at(
            &mut g,
            Evaluation::new(AttemptVerdict::Satisfied, "done"),
            at(2026, 7, 14, 9, 5),
        )
        .unwrap();
        assert_eq!(decision, ControllerDecision::Done);
        assert_eq!(g.state, GoalState::Done);
    }

    #[test]
    fn blocked_verdict_surfaces_the_block_with_its_reason() {
        let mut g = goal();
        let decision = apply_at(
            &mut g,
            Evaluation::new(AttemptVerdict::Blocked, "the 2024 statement is missing"),
            at(2026, 7, 14, 12, 0),
        )
        .unwrap();
        assert_eq!(decision, ControllerDecision::Blocked);
        assert_eq!(g.state, GoalState::Blocked);
        assert_eq!(g.note.as_deref(), Some("the 2024 statement is missing"));
    }

    #[test]
    fn needs_input_awaits_input() {
        let mut g = goal();
        let decision = apply_at(
            &mut g,
            Evaluation::new(
                AttemptVerdict::NeedsInput,
                "which bank account should I use?",
            ),
            at(2026, 7, 14, 12, 0),
        )
        .unwrap();
        assert_eq!(decision, ControllerDecision::AwaitInput);
        assert_eq!(g.state, GoalState::Blocked);
        assert_eq!(g.note.as_deref(), Some("which bank account should I use?"));
    }

    #[test]
    fn waiting_verdict_parks_the_goal_on_its_barrier() {
        use crate::waits::{WaitKind, WaitRequest};
        let mut g = goal();
        let request = WaitRequest::new(WaitKind::OutputPattern {
            handle: "pty-3".into(),
            pattern: "BUILD SUCCESSFUL".into(),
        })
        .with_timeout(at(2026, 7, 14, 18, 0));
        let decision = apply_at(
            &mut g,
            Evaluation::waiting("started the build in the background", request.clone()),
            at(2026, 7, 14, 12, 0),
        )
        .unwrap();

        assert_eq!(decision, ControllerDecision::Wait(request));
        assert_eq!(g.state, GoalState::Waiting);
        // The parking reason is the attempt summary, visible in listings.
        assert_eq!(
            g.note.as_deref(),
            Some("started the build in the background")
        );
        // The turn that set the barrier up still spent budget.
        assert_eq!(g.turns_used(), 1);
        assert_eq!(g.attempts.last().unwrap().verdict, AttemptVerdict::Waiting);
        // A parked goal cannot be evaluated again until it is woken.
        assert!(!GoalState::Waiting.can_transition_to(GoalState::Done));
    }

    #[test]
    fn waiting_without_a_barrier_surfaces_as_a_block() {
        let mut g = goal();
        // A Waiting verdict with no wait object is malformed — park on nothing
        // is never right, so it is surfaced as a block instead.
        let decision = apply_at(
            &mut g,
            Evaluation::new(AttemptVerdict::Waiting, "I'll just wait"),
            at(2026, 7, 14, 12, 0),
        )
        .unwrap();
        assert_eq!(decision, ControllerDecision::Blocked);
        assert_eq!(g.state, GoalState::Blocked);
        assert!(
            g.note.as_deref().unwrap().contains("named no barrier"),
            "{:?}",
            g.note
        );
    }

    #[test]
    fn waiting_past_the_deadline_fails_rather_than_parking() {
        use crate::waits::{WaitKind, WaitRequest};
        let mut g = Goal::new(
            "g",
            owner(),
            "obj",
            CompletionContract::new("out", "verify", "stop"),
            Budget::turns(5).with_deadline(at(2026, 7, 14, 18, 0)),
            at(2026, 7, 14, 9, 0),
        );
        g.begin_attempt(at(2026, 7, 14, 17, 59)).unwrap();
        let decision = g
            .apply_evaluation(
                Evaluation::waiting(
                    "waiting for CI",
                    WaitRequest::new(WaitKind::Event { key: "ci".into() }),
                ),
                at(2026, 7, 14, 18, 1),
            )
            .unwrap();
        // The hard envelope wins: a goal past its deadline does not park.
        assert_eq!(
            decision,
            ControllerDecision::Failed(FailureReason::DeadlinePassed)
        );
        assert_eq!(g.state, GoalState::Failed);
    }

    #[test]
    fn stop_condition_verdict_fails_the_goal_without_another_turn() {
        let mut g = goal();
        let decision = apply_at(
            &mut g,
            Evaluation::new(
                AttemptVerdict::Stopped,
                "the required document is unavailable",
            ),
            at(2026, 7, 14, 12, 0),
        )
        .unwrap();

        assert_eq!(
            decision,
            ControllerDecision::Failed(FailureReason::StopConditionMet)
        );
        assert_eq!(g.state, GoalState::Failed);
        assert_eq!(
            g.note.as_deref(),
            Some("the required document is unavailable")
        );
    }

    #[test]
    fn progressed_at_budget_exhaustion_fails() {
        let mut g = Goal::new(
            "g",
            owner(),
            "obj",
            CompletionContract::new("out", "verify", "stop"),
            Budget::turns(2),
            at(2026, 7, 14, 9, 0),
        );
        // First progressing turn: continue.
        assert_eq!(
            apply_at(
                &mut g,
                Evaluation::new(AttemptVerdict::Progressed, "step 1"),
                at(2026, 7, 14, 9, 5)
            )
            .unwrap(),
            ControllerDecision::Continue
        );
        // Second progressing turn spends the last of the budget: fail.
        let decision = apply_at(
            &mut g,
            Evaluation::new(AttemptVerdict::Progressed, "step 2"),
            at(2026, 7, 14, 9, 10),
        )
        .unwrap();
        assert_eq!(
            decision,
            ControllerDecision::Failed(FailureReason::TurnBudgetExhausted)
        );
        assert_eq!(g.state, GoalState::Failed);
        assert_eq!(g.turns_used(), 2);
        assert!(g.note.is_some());
    }

    #[test]
    fn progressed_past_deadline_fails_before_budget() {
        let mut g = Goal::new(
            "g",
            owner(),
            "obj",
            CompletionContract::new("out", "verify", "stop"),
            Budget::turns(10).with_deadline(at(2026, 7, 14, 18, 0)),
            at(2026, 7, 14, 9, 0),
        );
        g.begin_attempt(at(2026, 7, 14, 17, 59)).unwrap();
        let decision = g
            .apply_evaluation(
                Evaluation::new(AttemptVerdict::Progressed, "still working"),
                at(2026, 7, 14, 18, 1),
            )
            .unwrap();
        assert_eq!(
            decision,
            ControllerDecision::Failed(FailureReason::DeadlinePassed)
        );
        assert_eq!(g.state, GoalState::Failed);
    }

    #[test]
    fn cannot_evaluate_a_non_running_goal() {
        let mut g = goal();
        g.pause(at(2026, 7, 14, 10, 0)).unwrap();
        let err = g.apply_evaluation(
            Evaluation::new(AttemptVerdict::Progressed, "x"),
            at(2026, 7, 14, 10, 1),
        );
        assert!(err.is_err());
        // The rejected evaluation left the ledger untouched.
        assert_eq!(g.turns_used(), 0);
        assert_eq!(g.state, GoalState::Paused);
    }

    #[test]
    fn enforce_deadline_fails_a_waiting_goal_past_its_deadline() {
        let mut g = Goal::new(
            "g",
            owner(),
            "obj",
            CompletionContract::new("out", "verify", "stop"),
            Budget::turns(5).with_deadline(at(2026, 7, 14, 18, 0)),
            at(2026, 7, 14, 9, 0),
        );
        g.wait(at(2026, 7, 14, 10, 0)).unwrap();
        // Before the deadline: nothing fires.
        assert!(!g.enforce_deadline(at(2026, 7, 14, 17, 0)));
        assert_eq!(g.state, GoalState::Waiting);
        // Past it: the goal fails.
        assert!(g.enforce_deadline(at(2026, 7, 14, 18, 30)));
        assert_eq!(g.state, GoalState::Failed);
        assert!(g.note.is_some());
        // Idempotent on a terminal goal.
        assert!(!g.enforce_deadline(at(2026, 7, 14, 19, 0)));
    }

    #[test]
    fn lifecycle_edges_manage_the_note() {
        let mut g = goal();
        let t = at(2026, 7, 14, 10, 0);
        // Running -> Waiting -> Running (wake clears nothing, no note set).
        g.wait(t).unwrap();
        assert_eq!(g.state, GoalState::Waiting);
        g.wake(t).unwrap();
        assert_eq!(g.state, GoalState::Running);
        // Running -> Blocked (note set) -> resume clears note.
        g.block("waiting on a document", t).unwrap();
        assert_eq!(g.state, GoalState::Blocked);
        assert_eq!(g.note.as_deref(), Some("waiting on a document"));
        g.resume(t).unwrap();
        assert_eq!(g.state, GoalState::Running);
        assert_eq!(g.note, None);
        // Running -> Paused -> resume.
        g.pause(t).unwrap();
        assert_eq!(g.state, GoalState::Paused);
        g.resume(t).unwrap();
        assert_eq!(g.state, GoalState::Running);
        // fail from Running sets the note and is terminal.
        g.fail("user cancelled", t).unwrap();
        assert_eq!(g.state, GoalState::Failed);
        assert_eq!(g.note.as_deref(), Some("user cancelled"));
        // No lifecycle edge escapes a terminal state.
        assert!(g.resume(t).is_err());
        assert!(g.wake(t).is_err());
    }

    // ---- goal turn text ----------------------------------------------

    #[test]
    fn goal_turn_text_frames_the_contract_and_asks_for_evidence() {
        let mut g = Goal::new(
            "g",
            owner(),
            "prepare the 2025 tax return",
            CompletionContract {
                outcome: "a filled ELSTER draft".into(),
                verification: "the draft file validates".into(),
                constraints: vec!["keep the original intact".into()],
                boundaries: vec!["never submit, only prepare".into()],
                stop_condition: "give up if a required document is missing".into(),
            },
            Budget::turns(5),
            at(2026, 7, 14, 9, 0),
        );
        g.subgoals = vec![Subgoal::new("collect receipts")];

        let text = goal_turn_text(&g);
        assert!(text.starts_with(GOAL_PREFIX), "{text}");
        assert!(text.contains("prepare the 2025 tax return"), "{text}");
        assert!(text.contains("a filled ELSTER draft"), "{text}");
        assert!(text.contains("the draft file validates"), "{text}");
        assert!(
            text.contains("give up if a required document is missing"),
            "{text}"
        );
        assert!(text.contains("keep the original intact"), "{text}");
        assert!(text.contains("never submit, only prepare"), "{text}");
        assert!(text.contains("- [ ] collect receipts"), "{text}");
        assert!(text.contains("first of 5 attempts"), "{text}");
        assert!(text.contains("autonomously"), "{text}");
        // No silence token: the evaluator needs a factual summary.
        assert!(!text.contains("[SILENT]"), "{text}"); // the host's silence token
    }

    #[test]
    fn goal_turn_text_carries_the_last_attempt_as_continuity() {
        let mut g = goal(); // budget 5
        apply_at(
            &mut g,
            Evaluation::new(AttemptVerdict::Progressed, "downloaded three receipts"),
            at(2026, 7, 14, 10, 0),
        )
        .unwrap();
        let text = goal_turn_text(&g);
        assert!(text.contains("1 of 5 attempts used"), "{text}");
        assert!(text.contains("downloaded three receipts"), "{text}");
    }

    #[test]
    fn goal_turn_text_surfaces_a_wake_note() {
        let mut g = goal();
        // Park, then wake (as a resolved wait barrier would).
        g.wait(at(2026, 7, 14, 10, 0)).unwrap();
        g.wake(at(2026, 7, 14, 12, 0)).unwrap();
        g.note = Some("the build finished: exit status 0".into());
        let text = goal_turn_text(&g);
        assert!(
            text.contains("Update since you last worked on this: the build finished"),
            "{text}"
        );
        // A fresh Running goal (no note) shows no update line.
        assert!(!goal_turn_text(&goal()).contains("Update since"));
    }

    // ---- evaluator seam ----------------------------------------------

    struct ScriptedEvaluator(Evaluation);

    #[async_trait::async_trait]
    impl GoalEvaluator for ScriptedEvaluator {
        async fn evaluate(&self, _goal: &Goal, _turn: &TurnOutcome) -> anyhow::Result<Evaluation> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn evaluator_seam_feeds_the_controller() {
        let mut g = goal();
        let evaluator = ScriptedEvaluator(Evaluation::new(
            AttemptVerdict::Satisfied,
            "verified via the check command",
        ));
        let outcome = TurnOutcome {
            assistant_summary: "ran the verification".into(),
            artifacts: vec![],
            verification: vec!["exit 0".into()],
        };
        let eval = evaluator.evaluate(&g, &outcome).await.unwrap();
        let decision = apply_at(&mut g, eval, at(2026, 7, 14, 13, 0)).unwrap();
        assert_eq!(decision, ControllerDecision::Done);
        assert_eq!(g.state, GoalState::Done);
    }

    // ---- GoalStore persistence ---------------------------------------

    fn store() -> (GoalStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (GoalStore::new(dir.path().join("goals.json")), dir)
    }

    fn contract() -> CompletionContract {
        CompletionContract::new("out", "verify", "stop")
    }

    #[test]
    fn store_roundtrip_add_new_list_get_remove() {
        let (store, _dir) = store();
        assert!(store.list().unwrap().is_empty());

        let now = at(2026, 7, 14, 9, 0);
        let g = store
            .add_new(owner(), "prepare taxes", contract(), Budget::turns(5), now)
            .unwrap();
        assert_eq!(store.list().unwrap(), vec![g.clone()]);
        assert_eq!(store.get(&g.id).unwrap().as_ref(), Some(&g));
        assert_eq!(store.get("nope").unwrap(), None);

        assert!(store.remove(&g.id).unwrap());
        assert!(!store.remove(&g.id).unwrap());
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn add_new_assigns_unique_ids_within_a_second() {
        let (store, _dir) = store();
        let now = at(2026, 7, 14, 9, 0);
        let a = store
            .add_new(owner(), "a", contract(), Budget::turns(1), now)
            .unwrap();
        let b = store
            .add_new(owner(), "b", contract(), Budget::turns(1), now)
            .unwrap();
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn replacing_an_owner_never_reuses_the_old_goal_id() {
        let (store, _dir) = store();
        let now = at(2026, 7, 14, 9, 0);
        let old = store
            .add_new(owner(), "old", contract(), Budget::turns(1), now)
            .unwrap();

        let (new, replaced) = store
            .replace_current_for_owner(owner(), "new", contract(), Budget::turns(1), now)
            .unwrap();

        assert_eq!(replaced, 1);
        assert_ne!(old.id, new.id);
        assert_eq!(store.list().unwrap(), vec![new]);
    }

    #[test]
    fn replacing_and_removing_current_goals_preserves_terminal_history() {
        let (store, _dir) = store();
        let now = at(2026, 7, 14, 9, 0);
        let mut history = store
            .add_new(owner(), "finished", contract(), Budget::turns(1), now)
            .unwrap();
        history.fail("recorded", now).unwrap();
        history = store.update(&history).unwrap();
        let current = store
            .add_new(owner(), "current", contract(), Budget::turns(1), now)
            .unwrap();

        let (replacement, replaced) = store
            .replace_current_for_owner(owner(), "new", contract(), Budget::turns(1), now)
            .unwrap();
        assert_eq!(replaced, 1);
        assert!(store.get(&current.id).unwrap().is_none());
        assert_eq!(store.get(&history.id).unwrap(), Some(history.clone()));

        assert_eq!(store.remove_current_for_owner(&owner()).unwrap(), 1);
        assert!(store.get(&replacement.id).unwrap().is_none());
        assert_eq!(store.list().unwrap(), vec![history]);
        assert_eq!(store.remove_current_for_owner(&owner()).unwrap(), 0);
    }

    #[test]
    fn add_rejects_a_duplicate_id() {
        let (store, _dir) = store();
        let now = at(2026, 7, 14, 9, 0);
        let g = Goal::new("dup", owner(), "obj", contract(), Budget::turns(1), now);
        store.add(g.clone()).unwrap();
        assert!(store.add(g).is_err());
    }

    #[test]
    fn parallel_store_mutations_do_not_lose_goals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("goals.json");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let mut threads = Vec::new();
        for n in 0..8 {
            let path = path.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                let store = GoalStore::new(path);
                let now = at(2026, 7, 14, 9, 0);
                let goal = Goal::new(
                    format!("parallel-{n}"),
                    owner(),
                    format!("goal {n}"),
                    contract(),
                    Budget::turns(1),
                    now,
                );
                barrier.wait();
                store.add(goal)
            }));
        }

        for thread in threads {
            thread.join().unwrap().unwrap();
        }
        assert_eq!(GoalStore::new(path).list().unwrap().len(), 8);
    }

    #[test]
    fn update_persists_a_mutated_goal_and_rejects_unknown_ids() {
        let (store, _dir) = store();
        let now = at(2026, 7, 14, 9, 0);
        let mut g = store
            .add_new(owner(), "obj", contract(), Budget::turns(5), now)
            .unwrap();

        // Drive it through a turn, then persist the mutation.
        apply_at(
            &mut g,
            Evaluation::new(AttemptVerdict::Blocked, "need a document"),
            at(2026, 7, 14, 10, 0),
        )
        .unwrap();
        store.update(&g).unwrap();

        let reloaded = store.get(&g.id).unwrap().unwrap();
        assert_eq!(reloaded.state, GoalState::Blocked);
        assert_eq!(reloaded.note.as_deref(), Some("need a document"));
        assert_eq!(reloaded.attempts.len(), 1);

        // Updating a goal the store never saw is an error, not a silent create.
        let ghost = Goal::new("ghost", owner(), "x", contract(), Budget::turns(1), now);
        assert!(store.update(&ghost).is_err());
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn a_stale_goal_snapshot_cannot_overwrite_a_newer_user_transition() {
        let (store, _dir) = store();
        let now = at(2026, 7, 14, 9, 0);
        let original = store
            .add_new(owner(), "obj", contract(), Budget::turns(5), now)
            .unwrap();
        let mut controller_snapshot = original.clone();
        let mut user_snapshot = original;

        user_snapshot.pause(at(2026, 7, 14, 9, 1)).unwrap();
        store.update(&user_snapshot).unwrap();

        controller_snapshot
            .block("stale controller result", at(2026, 7, 14, 9, 2))
            .unwrap();
        let error = store.update(&controller_snapshot).unwrap_err();

        assert!(error.to_string().contains("revision conflict"), "{error}");
        assert_eq!(
            store.get(&controller_snapshot.id).unwrap().unwrap().state,
            GoalState::Paused
        );
    }

    #[test]
    fn claiming_an_attempt_is_revision_checked_and_persisted_atomically() {
        let (store, _dir) = store();
        let now = at(2026, 7, 14, 9, 0);
        let snapshot = store
            .add_new(owner(), "obj", contract(), Budget::turns(2), now)
            .unwrap();

        let claimed = store
            .claim_attempt(&snapshot, at(2026, 7, 14, 9, 1))
            .unwrap()
            .expect("fresh running snapshot should claim");

        assert_eq!(claimed.turns_used(), 1);
        assert!(claimed.in_flight.is_some());
        assert!(claimed.revision > snapshot.revision);
        assert_eq!(store.get(&snapshot.id).unwrap().unwrap(), claimed);
        assert!(store
            .claim_attempt(&snapshot, at(2026, 7, 14, 9, 2))
            .unwrap()
            .is_none());
    }

    #[test]
    fn an_undispatched_busy_claim_is_abandoned_without_spending_budget() {
        let (store, _dir) = store();
        let now = at(2026, 7, 14, 9, 0);
        let snapshot = store
            .add_new(owner(), "obj", contract(), Budget::turns(2), now)
            .unwrap();
        let claim = store
            .claim_attempt(&snapshot, at(2026, 7, 14, 9, 1))
            .unwrap()
            .unwrap();

        let abandoned = store.abandon_attempt(&claim).unwrap().unwrap();

        assert_eq!(abandoned.turns_used(), 0);
        assert!(abandoned.in_flight.is_none());
        assert_eq!(abandoned.state, GoalState::Running);
    }

    #[test]
    fn an_abandoned_claim_cannot_finish_a_replacement_with_the_same_timestamp() {
        let (store, _dir) = store();
        let now = at(2026, 7, 14, 9, 0);
        let snapshot = store
            .add_new(owner(), "obj", contract(), Budget::turns(2), now)
            .unwrap();
        let stale_claim = store.claim_attempt(&snapshot, now).unwrap().unwrap();
        let abandoned = store.abandon_attempt(&stale_claim).unwrap().unwrap();
        let replacement = store.claim_attempt(&abandoned, now).unwrap().unwrap();

        assert_ne!(stale_claim.in_flight, replacement.in_flight);
        assert!(store
            .finish_attempt(
                &stale_claim,
                AttemptCompletion::Evaluated(Evaluation::new(
                    AttemptVerdict::Satisfied,
                    "stale result",
                )),
                now,
            )
            .unwrap()
            .is_none());
        assert_eq!(
            store.get(&snapshot.id).unwrap().unwrap().in_flight,
            replacement.in_flight
        );
    }

    #[test]
    fn cancelling_during_a_turn_remains_terminal_when_the_result_arrives() {
        let (store, _dir) = store();
        let now = at(2026, 7, 14, 9, 0);
        let snapshot = store
            .add_new(owner(), "obj", contract(), Budget::turns(2), now)
            .unwrap();
        let claim = store.claim_attempt(&snapshot, now).unwrap().unwrap();
        let mut cancelled = store.get(&snapshot.id).unwrap().unwrap();
        cancelled.fail("cancelled by user", now).unwrap();
        store.update(&cancelled).unwrap();

        let (finished, decision) = store
            .finish_attempt(
                &claim,
                AttemptCompletion::Evaluated(Evaluation::new(
                    AttemptVerdict::Satisfied,
                    "late success",
                )),
                now,
            )
            .unwrap()
            .unwrap();

        assert_eq!(decision, ControllerDecision::Preempted);
        assert_eq!(finished.state, GoalState::Failed);
        assert_eq!(finished.note.as_deref(), Some("cancelled by user"));
        assert_eq!(finished.attempts.len(), 1);
        assert!(finished.in_flight.is_none());
    }

    #[test]
    fn active_and_active_for_owner_filter_out_terminal_goals() {
        let (store, _dir) = store();
        let now = at(2026, 7, 14, 9, 0);
        let other_owner = OwnerKey::from_parts(&["tui", "default"]);

        let running = store
            .add_new(owner(), "running", contract(), Budget::turns(5), now)
            .unwrap();
        let mut done = store
            .add_new(owner(), "done", contract(), Budget::turns(1), now)
            .unwrap();
        apply_at(
            &mut done,
            Evaluation::new(AttemptVerdict::Satisfied, "ok"),
            now,
        )
        .unwrap();
        store.update(&done).unwrap();
        let elsewhere = store
            .add_new(
                other_owner.clone(),
                "elsewhere",
                contract(),
                Budget::turns(5),
                now,
            )
            .unwrap();

        // active() drops the Done goal, keeps both Running ones.
        let mut active_ids: Vec<_> = store.active().unwrap().into_iter().map(|g| g.id).collect();
        active_ids.sort();
        let mut expected = vec![running.id.clone(), elsewhere.id.clone()];
        expected.sort();
        assert_eq!(active_ids, expected);

        // Scoped to a owner.
        let for_lane = store.active_for_owner(&owner()).unwrap();
        assert_eq!(for_lane.len(), 1);
        assert_eq!(for_lane[0].id, running.id);
    }

    #[test]
    fn preempt_owner_pauses_only_running_goals_of_that_lane() {
        let (store, _dir) = store();
        let now = at(2026, 7, 14, 9, 0);
        let other_owner = OwnerKey::from_parts(&["tui", "default"]);

        let running = store
            .add_new(owner(), "running", contract(), Budget::turns(5), now)
            .unwrap();
        // A blocked goal on the same owner stays blocked (already stopped).
        let mut blocked = store
            .add_new(owner(), "blocked", contract(), Budget::turns(5), now)
            .unwrap();
        blocked.block("waiting on a doc", now).unwrap();
        store.update(&blocked).unwrap();
        // A running goal on another owner is untouched.
        let elsewhere = store
            .add_new(
                other_owner.clone(),
                "elsewhere",
                contract(),
                Budget::turns(5),
                now,
            )
            .unwrap();

        let paused = store
            .preempt_owner(&owner(), at(2026, 7, 14, 10, 0))
            .unwrap();
        assert_eq!(paused, vec![running.id.clone()]);
        assert_eq!(
            store.get(&running.id).unwrap().unwrap().state,
            GoalState::Paused
        );
        assert_eq!(
            store.get(&blocked.id).unwrap().unwrap().state,
            GoalState::Blocked
        );
        assert_eq!(
            store.get(&elsewhere.id).unwrap().unwrap().state,
            GoalState::Running
        );

        // Idempotent: a second message preempts nothing new.
        assert!(store
            .preempt_owner(&owner(), at(2026, 7, 14, 10, 1))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn wake_waiting_returns_a_parked_goal_to_running_with_a_note() {
        let (store, _dir) = store();
        let now = at(2026, 7, 14, 9, 0);
        let mut g = store
            .add_new(owner(), "obj", contract(), Budget::turns(5), now)
            .unwrap();
        // Park it (as a Waiting-verdict turn would).
        g.wait(at(2026, 7, 14, 9, 30)).unwrap();
        g = store.update(&g).unwrap();

        let woke = store
            .wake_waiting(
                &g.id,
                Some("the build finished".into()),
                at(2026, 7, 14, 10, 0),
            )
            .unwrap();
        assert!(woke);
        let reloaded = store.get(&g.id).unwrap().unwrap();
        assert_eq!(reloaded.state, GoalState::Running);
        assert_eq!(reloaded.note.as_deref(), Some("the build finished"));
    }

    #[test]
    fn wake_waiting_is_a_no_op_for_a_goal_that_moved_on() {
        let (store, _dir) = store();
        let now = at(2026, 7, 14, 9, 0);
        let g = store
            .add_new(owner(), "obj", contract(), Budget::turns(5), now)
            .unwrap();
        // Still Running (never parked): waking is a no-op.
        assert!(!store
            .wake_waiting(&g.id, None, at(2026, 7, 14, 10, 0))
            .unwrap());
        assert_eq!(store.get(&g.id).unwrap().unwrap().state, GoalState::Running);
        // An unknown goal is a no-op too.
        assert!(!store
            .wake_waiting("ghost", None, at(2026, 7, 14, 10, 0))
            .unwrap());
    }

    #[test]
    fn goal_survives_a_json_roundtrip_with_full_ledger() {
        let (store, _dir) = store();
        let now = at(2026, 7, 14, 9, 0);
        let mut g = store
            .add_new(
                owner(),
                "obj",
                CompletionContract {
                    outcome: "out".into(),
                    verification: "verify".into(),
                    constraints: vec!["keep original".into()],
                    boundaries: vec!["never submit".into()],
                    stop_condition: "stop".into(),
                },
                Budget::turns(3).with_deadline(at(2026, 7, 20, 0, 0)),
                now,
            )
            .unwrap();
        g.subgoals = vec![Subgoal::new("collect receipts")];
        let mut eval = Evaluation::new(AttemptVerdict::Progressed, "did a thing");
        eval.artifacts = vec!["inbox/a.pdf".into()];
        eval.evidence = vec!["ok".into()];
        apply_at(&mut g, eval, at(2026, 7, 14, 10, 0)).unwrap();
        g = store.update(&g).unwrap();

        assert_eq!(store.get(&g.id).unwrap().unwrap(), g);
    }
}
