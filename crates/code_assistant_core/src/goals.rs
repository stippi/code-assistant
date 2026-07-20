//! Durable goals for ordinary code-assistant sessions — the host-side
//! consumer of the shared [`agent_orchestration`] goal domain.
//!
//! A goal is owned by the session that created it ([`session_owner`]) and
//! persisted in `goals.json` in the code-assistant data directory, so it
//! survives a session reload. Continuation is host-driven: while the app is
//! open, [`GoalController::pass`] drives each `Running` goal one bounded turn
//! at a time through [`SessionService::start_turn_if_idle`], evaluates the
//! typed [`TurnOutcome`](crate::session::TurnOutcome) with a
//! [`GoalEvaluator`], and folds the verdict back into the store. There is
//! deliberately no unattended auto-resume after the process ends — that
//! stronger contract (startup sweeps, orphan adoption, channel delivery)
//! belongs to hosts like pal.
//!
//! Invariants, enforced by the shared store and honoured here:
//! - claims precede work: [`GoalRepository::claim_attempt`] persists the
//!   in-flight marker before any turn is dispatched;
//! - every claimed attempt consumes budget or closes with an explicit
//!   abandonment (the sole abandonment path is an atomic `Busy` answer from
//!   `start_turn_if_idle`, where no work was dispatched);
//! - a stale in-flight marker from an earlier process is folded as an
//!   interrupted attempt — a crash cannot silently refund work;
//! - concurrent pause/cancel and turn completion merge through the store's
//!   revision/claim-token semantics instead of overwriting each other.

use crate::session::{SessionService, TurnDispatch, TurnRequest, TurnStatus};
use agent_core::ui::ToolStatus;
use agent_orchestration::goals::{
    goal_turn_text, AttemptCompletion, ControllerDecision, GoalRepository, GoalState, GoalStore,
    TurnOutcome as GoalTurnOutcome,
};
use agent_orchestration::waits::{WaitRepository, WaitStore};
use agent_orchestration::OwnerKey;
use anyhow::Result;
use chrono::NaiveDateTime;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};

// Re-exports so frontends can wire the controller without depending on
// agent_orchestration directly (mirrors pal's shim pattern).
pub use agent_orchestration::goal_eval::LlmGoalEvaluator;
pub use agent_orchestration::goals::{Goal, GoalEvaluator};

/// `goals.json` in the code-assistant data directory — next to `sessions/`,
/// because goals reference sessions, not projects.
pub fn default_goals_path() -> PathBuf {
    data_dir().join("goals.json")
}

/// `waits.json` in the code-assistant data directory: durable wait barriers
/// armed by a goal turn's `waiting` verdict.
pub fn default_waits_path() -> PathBuf {
    data_dir().join("waits.json")
}

fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("code-assistant")
}

/// The owner key of a session's goals. Namespaced so a store shared with
/// other hosts (which key by channel lane) stays unambiguous.
pub fn session_owner(session_id: &str) -> OwnerKey {
    OwnerKey::from_parts(&["session", session_id])
}

/// The session id behind a [`session_owner`] key; `None` for foreign owners.
pub fn owner_session_id(owner: &OwnerKey) -> Option<&str> {
    owner.as_str().strip_prefix("session:")
}

const MAX_TOOL_EVIDENCE_CHARS: usize = 4_000;

/// Map the typed session [`TurnOutcome`](crate::session::TurnOutcome) onto
/// the evaluator's structured input. The evidence is independent of the
/// assistant's narration: successful/error tool results, path-like inputs and
/// resource writes become verification and artifact evidence.
pub fn goal_turn_evidence(outcome: &crate::session::TurnOutcome) -> GoalTurnOutcome {
    let mut artifacts: Vec<String> = Vec::new();
    for resource in &outcome.resources_written {
        let artifact = if resource.project.is_empty() {
            resource.path.display().to_string()
        } else {
            format!("{}:{}", resource.project, resource.path.display())
        };
        if !artifacts.contains(&artifact) {
            artifacts.push(artifact);
        }
    }
    let verification = outcome
        .tools
        .iter()
        .filter(|tool| matches!(tool.status, ToolStatus::Success | ToolStatus::Error))
        .map(tool_evidence_line)
        .collect();
    GoalTurnOutcome {
        assistant_summary: outcome.final_response.trim().to_string(),
        artifacts,
        verification,
    }
}

/// One completed tool as an evidence line: `name [Status]: message`, its
/// path-like inputs, and a bounded output excerpt.
fn tool_evidence_line(tool: &crate::session::ToolRecord) -> String {
    let name = if tool.name.is_empty() {
        tool.tool_id.as_str()
    } else {
        tool.name.as_str()
    };
    let mut parts = vec![format!("{name} [{:?}]", tool.status)];
    let mut path_parameters: Vec<_> = tool
        .parameters
        .iter()
        .filter(|(name, value)| is_path_parameter(name) && !value.trim().is_empty())
        .map(|(name, value)| format!("{name}={}", value.trim()))
        .collect();
    path_parameters.sort();
    if !path_parameters.is_empty() {
        parts.push(format!("inputs: {}", path_parameters.join(", ")));
    }
    if let Some(message) = tool
        .message
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        parts[0].push_str(": ");
        parts[0].push_str(message);
    }
    if let Some(output) = tool
        .output
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        parts.push(truncate_chars(output, MAX_TOOL_EVIDENCE_CHARS));
    }
    parts.join("\n")
}

fn is_path_parameter(name: &str) -> bool {
    matches!(
        name,
        "path" | "file" | "file_path" | "filename" | "output_path"
    )
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(max).collect();
    truncated.push_str("…[truncated]");
    truncated
}

/// Drives the session-owned goals of one [`SessionService`]. One `pass`
/// resolves due clock waits, sweeps deadlines, and gives each `Running` goal
/// at most one bounded turn.
pub struct GoalController {
    service: SessionService,
    goals: Arc<dyn GoalRepository>,
    waits: Arc<dyn WaitRepository>,
    evaluator: Arc<dyn GoalEvaluator>,
}

impl GoalController {
    pub fn new(
        service: SessionService,
        goals: Arc<dyn GoalRepository>,
        waits: Arc<dyn WaitRepository>,
        evaluator: Arc<dyn GoalEvaluator>,
    ) -> Self {
        Self {
            service,
            goals,
            waits,
            evaluator,
        }
    }

    /// Controller over the JSON stores at the given paths.
    pub fn with_stores(
        service: SessionService,
        goals_path: impl Into<PathBuf>,
        waits_path: impl Into<PathBuf>,
        evaluator: Arc<dyn GoalEvaluator>,
    ) -> Self {
        Self::new(
            service,
            Arc::new(GoalStore::new(goals_path.into())),
            Arc::new(WaitStore::new(waits_path.into())),
            evaluator,
        )
    }

    /// One sweep at the local wall clock.
    pub async fn pass(&self) -> Result<()> {
        self.pass_at(chrono::Local::now().naive_local()).await
    }

    /// One sweep at a fixed `now` — deterministic for tests and hosts with
    /// their own clock.
    pub async fn pass_at(&self, now: NaiveDateTime) -> Result<()> {
        self.wait_pass(now)?;
        self.goal_pass(now).await
    }

    /// Resolve armed wait barriers: clock barriers that came due are
    /// satisfied, expired timeouts fire, and barriers whose goal is gone or
    /// no longer waiting are cancelled. Non-clock barriers (events, jobs,
    /// sub-agents) have no runtime probes in code-assistant — they park the
    /// goal until their timeout fires or the user resumes it.
    fn wait_pass(&self, now: NaiveDateTime) -> Result<()> {
        for mut wait in self.waits.armed()? {
            let goal = self.goals.get(&wait.goal_id)?;
            let still_waiting = goal
                .as_ref()
                .is_some_and(|goal| goal.state == GoalState::Waiting);
            if !still_waiting {
                // The goal moved on (resumed, cancelled, done) — the barrier
                // is stale and must not wake anything later.
                if wait.cancel("goal no longer waiting", now) {
                    let _ = self.waits.update(&wait);
                }
                continue;
            }
            if wait.timed_out(now) {
                wait.time_out(now)?;
            } else if wait.due(now) {
                wait.satisfy(Some("the time arrived".into()), now)?;
            } else {
                continue;
            }
            self.waits.update(&wait)?;
            self.goals
                .wake_waiting(&wait.goal_id, wait.note.clone(), now)?;
        }
        Ok(())
    }

    async fn goal_pass(&self, now: NaiveDateTime) -> Result<()> {
        for snapshot in self.goals.active()? {
            let Some(session_id) = owner_session_id(&snapshot.owner).map(str::to_string) else {
                // Owned by another host (e.g. a channel lane) — not ours.
                continue;
            };
            if let Err(error) = self.drive_goal(snapshot, &session_id, now).await {
                warn!("goal pass: {error:#}");
            }
        }
        Ok(())
    }

    async fn drive_goal(&self, snapshot: Goal, session_id: &str, now: NaiveDateTime) -> Result<()> {
        // A leftover in-flight marker means an earlier process died mid-turn:
        // close it as an interrupted attempt so the crash cannot refund the
        // claimed budget. (Within one process, passes run sequentially and
        // every dispatched turn is awaited, so a live claim never appears
        // here.)
        if snapshot.in_flight.is_some() {
            self.goals.finish_attempt(
                &snapshot,
                AttemptCompletion::ControllerError(
                    "attempt interrupted (process ended mid-turn)".into(),
                ),
                now,
            )?;
            return Ok(());
        }

        // Deadline sweep for goals that are parked and will not take a turn.
        if snapshot.state != GoalState::Running {
            let mut goal = snapshot;
            if goal.enforce_deadline(now) {
                self.goals.update(&goal)?;
            }
            return Ok(());
        }

        // Cheap busy gate to avoid claim/abandon churn; `start_turn_if_idle`
        // below stays the authoritative, atomic idle check.
        if self.service.is_session_busy(session_id.to_string()).await? {
            return Ok(());
        }

        let Some(claim) = self.goals.claim_attempt(&snapshot, now)? else {
            // Stale snapshot, or the claim itself resolved the goal
            // (deadline/budget) — nothing was dispatched.
            return Ok(());
        };

        let dispatch = self
            .service
            .start_turn_if_idle(
                session_id.to_string(),
                TurnRequest::text(goal_turn_text(&claim)),
            )
            .await;
        let handle = match dispatch {
            Ok(TurnDispatch::Started(handle)) => handle,
            Ok(TurnDispatch::Busy) => {
                // No work was dispatched — the only path that releases a
                // claim without spending budget.
                self.goals.abandon_attempt(&claim)?;
                return Ok(());
            }
            Err(error) => {
                self.goals.finish_attempt(
                    &claim,
                    AttemptCompletion::ControllerError(format!("turn dispatch failed: {error:#}")),
                    now,
                )?;
                return Ok(());
            }
        };

        let outcome = match handle.wait().await {
            Ok(outcome) => outcome,
            Err(error) => {
                self.goals.finish_attempt(
                    &claim,
                    AttemptCompletion::ControllerError(format!("turn outcome lost: {error:#}")),
                    now,
                )?;
                return Ok(());
            }
        };

        if let TurnStatus::Failed { error } = &outcome.status {
            self.goals.finish_attempt(
                &claim,
                AttemptCompletion::ControllerError(format!("turn failed: {error}")),
                now,
            )?;
            return Ok(());
        }

        // A user message absorbed mid-turn does NOT pause the goal — the
        // user's turn simply takes natural priority and the goal continues
        // afterwards (`/goal pause|cancel` is the deliberate stop). Only an
        // explicit run cancel is a stop signal strong enough to park the
        // session's goals until the user resumes them.
        let user_took_over = outcome.status == TurnStatus::Cancelled;

        let evidence = goal_turn_evidence(&outcome);
        let completion = match self.evaluator.evaluate(&claim, &evidence).await {
            Ok(evaluation) => AttemptCompletion::Evaluated(evaluation),
            Err(error) => {
                AttemptCompletion::ControllerError(format!("goal evaluation failed: {error:#}"))
            }
        };

        if let Some((goal, decision)) = self.goals.finish_attempt(&claim, completion, now)? {
            debug!("goal {}: {:?}", goal.id, decision);
            if let ControllerDecision::Wait(request) = decision {
                self.waits.arm(
                    goal.id.clone(),
                    goal.owner.clone(),
                    request.kind,
                    request.timeout,
                    now,
                )?;
            }
        }

        if user_took_over {
            self.goals.preempt_owner(&claim.owner, now)?;
        }
        Ok(())
    }
}

/// Spawn the periodic controller loop (host-driven continuation: it lives and
/// dies with the process). Modeled on
/// [`spawn_wakeup_scheduler`](crate::session::spawn_wakeup_scheduler).
pub fn spawn_goal_controller(
    controller: GoalController,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(error) = controller.pass().await {
                debug!("goal controller pass failed: {error:#}");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::FileSessionPersistence;
    use crate::session::service::{
        default_project_manager_factory, AgentRuntimeOptions, LlmClientFactory,
    };
    use crate::session::{SessionConfig, SessionManager};
    use agent_orchestration::goals::{
        AttemptVerdict, Budget, CompletionContract, Evaluation, GoalStore,
    };
    use agent_orchestration::waits::{WaitKind, WaitRequest, WaitState, WaitStore};
    use chrono::NaiveDate;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use tokio::sync::Mutex as AsyncMutex;

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 7, 16)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
    }

    /// Evaluator that replays a scripted verdict sequence — the deterministic
    /// stand-in for the LLM judge.
    struct ScriptedEvaluator {
        script: Mutex<VecDeque<Evaluation>>,
        seen: Mutex<Vec<GoalTurnOutcome>>,
    }

    impl ScriptedEvaluator {
        fn new(script: Vec<Evaluation>) -> Self {
            Self {
                script: Mutex::new(script.into()),
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl GoalEvaluator for ScriptedEvaluator {
        async fn evaluate(&self, _goal: &Goal, turn: &GoalTurnOutcome) -> Result<Evaluation> {
            self.seen.lock().unwrap().push(turn.clone());
            self.script
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("scripted evaluator ran dry"))
        }
    }

    /// Session service whose agent runs use the injected LLM factory (same
    /// wiring as the service's own turn tests).
    fn test_service(
        root: &std::path::Path,
        factory: LlmClientFactory,
    ) -> (SessionService, Arc<AsyncMutex<SessionManager>>) {
        let events = crate::session::event_stream::EventStream::new();
        let persistence = FileSessionPersistence::new_with_root_dir(root.to_path_buf());
        let manager = Arc::new(AsyncMutex::new(SessionManager::new(
            persistence,
            SessionConfig::default(),
            "test-model".to_string(),
            crate::tools::test_registry(),
            events.clone(),
        )));
        let runtime = Arc::new(AgentRuntimeOptions {
            record_path: None,
            playback_path: None,
            fast_playback: false,
            command_executor_factory: Arc::new(|_| {
                Box::new(crate::mocks::create_command_executor_mock())
            }),
            project_manager_factory: default_project_manager_factory(),
            llm_client_factory: Some(factory),
        });
        let (service, worker) = SessionService::new(manager.clone(), runtime, events);
        tokio::spawn(worker);
        (service, manager)
    }

    /// Streams its scripted text through the callback like a real provider,
    /// so the turn's `final_response` (the evaluator's summary input) is
    /// populated. Same shape as the service's own turn-test provider.
    struct StreamingScriptedProvider {
        text: String,
    }

    #[async_trait::async_trait]
    impl llm::LLMProvider for StreamingScriptedProvider {
        async fn send_message(
            &mut self,
            _request: llm::LLMRequest,
            streaming_callback: Option<&llm::StreamingCallback>,
        ) -> Result<llm::LLMResponse> {
            if let Some(callback) = streaming_callback {
                callback(&llm::StreamingChunk::Text(self.text.clone()))?;
                callback(&llm::StreamingChunk::StreamingComplete)?;
            }
            Ok(llm::LLMResponse {
                content: vec![llm::ContentBlock::new_text(&self.text)],
                usage: llm::Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                rate_limit_info: None,
            })
        }
    }

    fn completing_llm(text: &'static str) -> LlmClientFactory {
        Arc::new(move |_model| {
            Ok(Box::new(StreamingScriptedProvider {
                text: text.to_string(),
            }))
        })
    }

    struct Fixture {
        service: SessionService,
        manager: Arc<AsyncMutex<SessionManager>>,
        goals: Arc<GoalStore>,
        waits: Arc<WaitStore>,
        _tmp: tempfile::TempDir,
    }

    impl Fixture {
        async fn new(factory: LlmClientFactory) -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let (service, manager) = test_service(tmp.path(), factory);
            let goals = Arc::new(GoalStore::new(tmp.path().join("goals.json")));
            let waits = Arc::new(WaitStore::new(tmp.path().join("waits.json")));
            Self {
                service,
                manager,
                goals,
                waits,
                _tmp: tmp,
            }
        }

        fn controller(&self, evaluator: Arc<dyn GoalEvaluator>) -> GoalController {
            GoalController::new(
                self.service.clone(),
                self.goals.clone(),
                self.waits.clone(),
                evaluator,
            )
        }

        async fn session_with_goal(&self, max_turns: u32) -> (String, Goal) {
            let session_id = self.service.create_session(None, None).await.unwrap();
            let goal = self
                .goals
                .add_new(
                    session_owner(&session_id),
                    "ship the widget",
                    CompletionContract::new("widget shipped", "check the registry", "give up"),
                    Budget::turns(max_turns),
                    now(),
                )
                .unwrap();
            (session_id, goal)
        }
    }

    #[test]
    fn owner_key_roundtrips_the_session_id() {
        let owner = session_owner("sess-42");
        assert_eq!(owner.as_str(), "session:sess-42");
        assert_eq!(owner_session_id(&owner), Some("sess-42"));
        assert_eq!(
            owner_session_id(&OwnerKey::from_parts(&["lane", "x"])),
            None
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_pass_drives_a_running_goal_and_a_satisfied_verdict_completes_it() {
        let fx = Fixture::new(completing_llm("Shipped it; registry updated.")).await;
        let (_, goal) = fx.session_with_goal(3).await;
        let evaluator = Arc::new(ScriptedEvaluator::new(vec![Evaluation::new(
            AttemptVerdict::Satisfied,
            "the registry lists the widget",
        )]));

        fx.controller(evaluator.clone())
            .pass_at(now())
            .await
            .unwrap();

        let goal = fx.goals.get(&goal.id).unwrap().unwrap();
        assert_eq!(goal.state, GoalState::Done);
        assert_eq!(goal.attempts.len(), 1);
        assert_eq!(goal.attempts[0].verdict, AttemptVerdict::Satisfied);
        assert!(goal.in_flight.is_none());
        // The evaluator judged the typed outcome, not an event-stream guess.
        let seen = evaluator.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].assistant_summary, "Shipped it; registry updated.");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_busy_session_is_skipped_without_spending_budget() {
        let fx = Fixture::new(completing_llm("unused")).await;
        let (session_id, goal) = fx.session_with_goal(3).await;
        {
            let mut manager = fx.manager.lock().await;
            manager
                .get_session_mut(&session_id)
                .unwrap()
                .set_activity_state(crate::session::instance::SessionActivityState::AgentRunning);
        }
        let evaluator = Arc::new(ScriptedEvaluator::new(vec![]));

        fx.controller(evaluator).pass_at(now()).await.unwrap();

        let goal = fx.goals.get(&goal.id).unwrap().unwrap();
        assert_eq!(goal.state, GoalState::Running);
        assert!(goal.attempts.is_empty(), "no budget may be spent");
        assert!(goal.in_flight.is_none(), "no claim may linger");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failing_turn_spends_budget_as_a_controller_error() {
        let fx = Fixture::new(Arc::new(|_model| {
            Ok(Box::new(crate::mocks::MockLLMProvider::new(vec![Err(
                anyhow::anyhow!("model exploded"),
            )])))
        }))
        .await;
        let (_, goal) = fx.session_with_goal(3).await;
        let evaluator = Arc::new(ScriptedEvaluator::new(vec![]));

        fx.controller(evaluator).pass_at(now()).await.unwrap();

        let goal = fx.goals.get(&goal.id).unwrap().unwrap();
        assert_eq!(
            goal.state,
            GoalState::Running,
            "budget remains — retry allowed"
        );
        assert_eq!(goal.attempts.len(), 1);
        assert_eq!(goal.attempts[0].verdict, AttemptVerdict::Error);
        assert!(goal.attempts[0].summary.contains("model exploded"));
        assert!(goal.in_flight.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn budget_exhaustion_fails_a_merely_progressing_goal() {
        let fx = Fixture::new(completing_llm("made some progress")).await;
        let (_, goal) = fx.session_with_goal(1).await;
        let evaluator = Arc::new(ScriptedEvaluator::new(vec![Evaluation::new(
            AttemptVerdict::Progressed,
            "one step further",
        )]));

        fx.controller(evaluator).pass_at(now()).await.unwrap();

        let goal = fx.goals.get(&goal.id).unwrap().unwrap();
        assert_eq!(goal.state, GoalState::Failed);
        assert_eq!(goal.note.as_deref(), Some("turn budget exhausted"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stale_in_flight_claim_is_folded_as_an_interrupted_attempt() {
        let fx = Fixture::new(completing_llm("unused")).await;
        let (_, goal) = fx.session_with_goal(3).await;
        // Simulate a process that died mid-turn: the claim is persisted but
        // no turn is running.
        fx.goals.claim_attempt(&goal, now()).unwrap().unwrap();
        let evaluator = Arc::new(ScriptedEvaluator::new(vec![]));

        fx.controller(evaluator).pass_at(now()).await.unwrap();

        let goal = fx.goals.get(&goal.id).unwrap().unwrap();
        assert!(goal.in_flight.is_none());
        assert_eq!(
            goal.attempts.len(),
            1,
            "the crash cannot refund the claimed turn"
        );
        assert_eq!(goal.attempts[0].verdict, AttemptVerdict::Error);
        assert_eq!(goal.state, GoalState::Running);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_waiting_verdict_parks_the_goal_and_a_due_clock_barrier_wakes_it() {
        let fx = Fixture::new(Arc::new(|_model| {
            // One scripted turn per pass that reaches the model.
            Ok(Box::new(crate::mocks::MockLLMProvider::new(vec![Ok(
                crate::mocks::create_test_response_text("waiting for the window"),
            )])))
        }))
        .await;
        let (_, goal) = fx.session_with_goal(3).await;
        let wake_at = now() + chrono::Duration::hours(1);
        let evaluator = Arc::new(ScriptedEvaluator::new(vec![
            Evaluation::waiting(
                "the deploy window opens later",
                WaitRequest {
                    kind: WaitKind::Until { at: wake_at },
                    timeout: None,
                },
            ),
            Evaluation::new(AttemptVerdict::Satisfied, "window used, contract verified"),
        ]));
        let controller = fx.controller(evaluator);

        controller.pass_at(now()).await.unwrap();
        let parked = fx.goals.get(&goal.id).unwrap().unwrap();
        assert_eq!(parked.state, GoalState::Waiting);
        let armed = fx.waits.armed_for_goal(&goal.id).unwrap();
        assert_eq!(armed.len(), 1);

        // Before the barrier is due nothing moves (and no budget is spent).
        controller
            .pass_at(now() + chrono::Duration::minutes(30))
            .await
            .unwrap();
        assert_eq!(
            fx.goals.get(&goal.id).unwrap().unwrap().state,
            GoalState::Waiting
        );

        // Past the barrier the wait fires and the same pass drives the goal on.
        controller
            .pass_at(wake_at + chrono::Duration::minutes(1))
            .await
            .unwrap();
        let woken = fx.goals.get(&goal.id).unwrap().unwrap();
        assert_eq!(woken.state, GoalState::Done);
        assert_eq!(
            fx.waits.get(&armed[0].id).unwrap().unwrap().state,
            WaitState::Satisfied
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_deadline_fails_a_parked_goal_without_a_turn() {
        let fx = Fixture::new(completing_llm("unused")).await;
        let session_id = fx.service.create_session(None, None).await.unwrap();
        let deadline = now() + chrono::Duration::hours(1);
        let goal = fx
            .goals
            .add_new(
                session_owner(&session_id),
                "beat the clock",
                CompletionContract::new("done", "check", "stop"),
                Budget::turns(3).with_deadline(deadline),
                now(),
            )
            .unwrap();
        let mut paused = goal.clone();
        paused.pause(now()).unwrap();
        let paused = fx.goals.update(&paused).unwrap();
        let evaluator = Arc::new(ScriptedEvaluator::new(vec![]));

        fx.controller(evaluator)
            .pass_at(deadline + chrono::Duration::minutes(1))
            .await
            .unwrap();

        let goal = fx.goals.get(&paused.id).unwrap().unwrap();
        assert_eq!(goal.state, GoalState::Failed);
        assert_eq!(goal.note.as_deref(), Some("deadline passed"));
    }

    #[test]
    fn evidence_maps_tools_and_resources_from_the_typed_outcome() {
        let outcome = crate::session::TurnOutcome {
            turn_id: 1,
            status: TurnStatus::Completed,
            final_response: "  did the thing  ".into(),
            tools: vec![crate::session::ToolRecord {
                tool_id: "t1".into(),
                name: "write_file".into(),
                status: ToolStatus::Success,
                message: Some("wrote it".into()),
                output: Some("ok".into()),
                parameters: vec![("path".into(), "src/lib.rs".into())],
            }],
            resources_written: vec![crate::session::ResourceRef {
                project: "widget".into(),
                path: PathBuf::from("src/lib.rs"),
            }],
            user_preempted: false,
            usage: Default::default(),
        };

        let evidence = goal_turn_evidence(&outcome);
        assert_eq!(evidence.assistant_summary, "did the thing");
        assert_eq!(evidence.artifacts, vec!["widget:src/lib.rs".to_string()]);
        assert_eq!(evidence.verification.len(), 1);
        assert!(evidence.verification[0].contains("write_file [Success]: wrote it"));
        assert!(evidence.verification[0].contains("inputs: path=src/lib.rs"));
    }
}
