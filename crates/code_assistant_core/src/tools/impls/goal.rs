//! The `goal` tool: the agent's access to durable goals (see
//! [`crate::goals`]). A goal is an outcome that stays on record across
//! session reloads; while the app is open, the goal controller drives it
//! turn by turn against its completion contract. Owner is the session the
//! tool runs in — every action is scoped to that session's goals.
//!
//! Stateless over `goals.json`: each invocation loads, mutates, saves. The
//! store's optimistic revisions keep concurrent controller turns and user
//! lifecycle edits from overwriting each other.

use crate::goals::{default_goals_path, session_owner};
use crate::tools::core::{
    capabilities, Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolSpec,
};
use agent_orchestration::goals::{Budget, CompletionContract, Goal, GoalState, GoalStore, Subgoal};
use anyhow::{anyhow, Result};
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalAction {
    /// Commit this session to a new durable goal.
    Create,
    /// One goal in full detail: contract, budget, attempt ledger.
    Show,
    /// List this session's goals with their state and progress.
    List,
    /// Pause an active goal (the controller stops driving it).
    Pause,
    /// Resume a paused or blocked goal (the controller drives it again).
    Resume,
    /// Give up on a goal; it becomes terminal but stays on the ledger.
    Cancel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalInput {
    pub action: GoalAction,
    /// For `create`: the user's objective, in their terms.
    #[serde(default)]
    pub objective: Option<String>,
    /// For `create`: what "done" means — the outcome to reach.
    #[serde(default)]
    pub outcome: Option<String>,
    /// For `create`: how success is verified (a command, a check, an
    /// artifact's existence). The goal reaches "done" only when this passes.
    #[serde(default)]
    pub verification: Option<String>,
    /// For `create`: when to give up rather than keep trying.
    #[serde(default)]
    pub stop_condition: Option<String>,
    /// For `create`, optional: things that must hold throughout.
    #[serde(default)]
    pub constraints: Option<Vec<String>>,
    /// For `create`, optional: hard limits the agent may never cross.
    #[serde(default)]
    pub boundaries: Option<Vec<String>>,
    /// For `create`, optional: a starting checklist of steps.
    #[serde(default)]
    pub subgoals: Option<Vec<String>>,
    /// For `create`: the maximum number of autonomous turns to spend.
    #[serde(default)]
    pub max_turns: Option<u32>,
    /// For `create`, optional: a wall-clock deadline `YYYY-MM-DD HH:MM`; past
    /// it the goal fails.
    #[serde(default)]
    pub deadline: Option<String>,
    /// For `show`/`pause`/`resume`/`cancel`: the goal id.
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalOutput {
    pub success: bool,
    pub message: String,
}

impl Render for GoalOutput {
    fn status(&self) -> String {
        self.message.lines().next().unwrap_or_default().to_string()
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        self.message.clone()
    }
}

impl ToolResult for GoalOutput {
    fn is_success(&self) -> bool {
        self.success
    }
}

/// Stateless over `goals.json`; the store's per-path lock and revisions keep
/// invocations and controller turns consistent.
pub struct GoalTool {
    goals_path: PathBuf,
    /// Fixed "now" for tests; `None` uses the local clock.
    now_override: Option<NaiveDateTime>,
}

impl Default for GoalTool {
    fn default() -> Self {
        Self::new(default_goals_path())
    }
}

impl GoalTool {
    pub fn new(goals_path: impl Into<PathBuf>) -> Self {
        Self {
            goals_path: goals_path.into(),
            now_override: None,
        }
    }

    #[cfg(test)]
    fn with_now(mut self, now: NaiveDateTime) -> Self {
        self.now_override = Some(now);
        self
    }

    fn now(&self) -> NaiveDateTime {
        self.now_override
            .unwrap_or_else(|| chrono::Local::now().naive_local())
    }

    fn store(&self) -> GoalStore {
        GoalStore::new(&self.goals_path)
    }

    fn create(&self, session_id: &str, input: &GoalInput) -> Result<GoalOutput> {
        let objective = require(&input.objective, "objective")?;
        let outcome = require(&input.outcome, "outcome")?;
        let verification = require(&input.verification, "verification")?;
        let stop_condition = require(&input.stop_condition, "stop_condition")?;
        let max_turns = input
            .max_turns
            .ok_or_else(|| anyhow!("'create' needs 'max_turns' (the autonomous turn budget)"))?;
        if max_turns == 0 {
            return Err(anyhow!("'max_turns' must be at least 1"));
        }

        let mut contract = CompletionContract::new(outcome, verification, stop_condition);
        contract.constraints = clean(input.constraints.as_deref());
        contract.boundaries = clean(input.boundaries.as_deref());

        let mut budget = Budget::turns(max_turns);
        if let Some(deadline) = input
            .deadline
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            let dl = NaiveDateTime::parse_from_str(deadline, "%Y-%m-%d %H:%M")
                .map_err(|_| anyhow!("'deadline' must be local time formatted YYYY-MM-DD HH:MM"))?;
            if dl <= self.now() {
                return Err(anyhow!("'deadline' is in the past ({dl})"));
            }
            budget = budget.with_deadline(dl);
        }

        let store = self.store();
        let mut goal = store.add_new(
            session_owner(session_id),
            objective,
            contract,
            budget,
            self.now(),
        )?;

        let subgoals = clean(input.subgoals.as_deref());
        if !subgoals.is_empty() {
            goal.subgoals = subgoals.into_iter().map(Subgoal::new).collect();
            goal = store.update(&goal)?;
        }

        let deadline_note = goal
            .budget
            .deadline
            .map(|dl| format!(", deadline {}", dl.format("%Y-%m-%d %H:%M")))
            .unwrap_or_default();
        Ok(GoalOutput {
            success: true,
            message: format!(
                "Committed goal {} (budget {} turns{}). While the app is open, the goal \
                 controller pursues it autonomously against its completion contract until it is \
                 satisfied, blocked, or the budget runs out; it stays on record across session \
                 reloads (but is not pursued while the app is closed).",
                goal.id, goal.budget.max_turns, deadline_note,
            ),
        })
    }

    fn show(&self, session_id: &str, input: &GoalInput) -> Result<GoalOutput> {
        let goal = self.load_owned(session_id, input)?;
        let mut lines = vec![
            format!(
                "Goal {} [{}]: {}",
                goal.id,
                goal.state.label(),
                goal.objective
            ),
            format!("- Done when: {}", goal.contract.outcome),
            format!("- Verify by: {}", goal.contract.verification),
            format!("- Stop if: {}", goal.contract.stop_condition),
        ];
        for c in &goal.contract.constraints {
            lines.push(format!("- Constraint: {c}"));
        }
        for b in &goal.contract.boundaries {
            lines.push(format!("- Boundary: {b}"));
        }
        let deadline = goal
            .budget
            .deadline
            .map(|dl| format!(", deadline {}", dl.format("%Y-%m-%d %H:%M")))
            .unwrap_or_default();
        lines.push(format!(
            "- Budget: {}/{} turns used{}",
            goal.turns_used(),
            goal.budget.max_turns,
            deadline,
        ));
        if !goal.subgoals.is_empty() {
            lines.push("Checklist:".to_string());
            for s in &goal.subgoals {
                lines.push(format!(
                    "- [{}] {}",
                    if s.done { "x" } else { " " },
                    s.description
                ));
            }
        }
        if let Some(note) = &goal.note {
            lines.push(format!("Note: {note}"));
        }
        if !goal.attempts.is_empty() {
            lines.push("Recent attempts:".to_string());
            for attempt in goal.attempts.iter().rev().take(5).rev() {
                lines.push(format!(
                    "- {} [{:?}] {}",
                    attempt.at.format("%Y-%m-%d %H:%M"),
                    attempt.verdict,
                    attempt.summary,
                ));
            }
        }
        Ok(GoalOutput {
            success: true,
            message: lines.join("\n"),
        })
    }

    fn list(&self, session_id: &str) -> Result<GoalOutput> {
        let owner = session_owner(session_id);
        let mut goals: Vec<Goal> = self
            .store()
            .list()?
            .into_iter()
            .filter(|g| g.owner == owner)
            .collect();
        if goals.is_empty() {
            return Ok(GoalOutput {
                success: true,
                message: "No goals on record for this session.".to_string(),
            });
        }
        // Active goals first, then terminal ones; stable within each group.
        goals.sort_by_key(|g| g.state.is_terminal());
        let mut lines = vec![format!("{} goal(s):", goals.len())];
        for g in goals {
            let note = g
                .note
                .as_deref()
                .map(|n| format!(" — {n}"))
                .unwrap_or_default();
            lines.push(format!(
                "- {} [{}] {}/{} turns: {}{}",
                g.id,
                g.state.label(),
                g.turns_used(),
                g.budget.max_turns,
                g.objective,
                note,
            ));
        }
        Ok(GoalOutput {
            success: true,
            message: lines.join("\n"),
        })
    }

    fn pause(&self, session_id: &str, input: &GoalInput) -> Result<GoalOutput> {
        let mut goal = self.load_owned(session_id, input)?;
        if goal.state == GoalState::Paused {
            return Ok(GoalOutput {
                success: true,
                message: format!("Goal {} is already paused.", goal.id),
            });
        }
        goal.pause(self.now())
            .map_err(|_| finished_error(&goal.id, goal.state))?;
        self.store().update(&goal)?;
        Ok(GoalOutput {
            success: true,
            message: format!(
                "Paused goal {}. Resume it to have the controller drive it again.",
                goal.id
            ),
        })
    }

    fn resume(&self, session_id: &str, input: &GoalInput) -> Result<GoalOutput> {
        let mut goal = self.load_owned(session_id, input)?;
        if goal.state == GoalState::Running {
            return Ok(GoalOutput {
                success: true,
                message: format!("Goal {} is already running.", goal.id),
            });
        }
        goal.resume(self.now())
            .map_err(|_| finished_error(&goal.id, goal.state))?;
        self.store().update(&goal)?;
        Ok(GoalOutput {
            success: true,
            message: format!(
                "Resumed goal {}; the controller will drive it on the next pass.",
                goal.id
            ),
        })
    }

    fn cancel(&self, session_id: &str, input: &GoalInput) -> Result<GoalOutput> {
        let mut goal = self.load_owned(session_id, input)?;
        goal.fail("cancelled by the user", self.now())
            .map_err(|_| finished_error(&goal.id, goal.state))?;
        self.store().update(&goal)?;
        Ok(GoalOutput {
            success: true,
            message: format!(
                "Cancelled goal {}; it stays on the ledger as failed.",
                goal.id
            ),
        })
    }

    /// Load a goal by id and verify it belongs to this session — one
    /// session's agent must not steer another session's goals.
    fn load_owned(&self, session_id: &str, input: &GoalInput) -> Result<Goal> {
        let id = require(&input.id, "id")?;
        let goal = self
            .store()
            .get(&id)?
            .ok_or_else(|| anyhow!("no goal with id {id}"))?;
        if goal.owner != session_owner(session_id) {
            return Err(anyhow!("goal {id} belongs to another session"));
        }
        Ok(goal)
    }
}

fn require(field: &Option<String>, name: &str) -> Result<String> {
    field
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("'{name}' is required and must be non-empty"))
}

fn clean(items: Option<&[String]>) -> Vec<String> {
    items
        .unwrap_or_default()
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn finished_error(id: &str, state: GoalState) -> anyhow::Error {
    anyhow!("goal {id} is already {} and cannot change", state.label())
}

#[async_trait::async_trait]
impl Tool for GoalTool {
    type Input = GoalInput;
    type Output = GoalOutput;

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "goal".into(),
            description: "Commit this session to a durable goal: an outcome pursued \
                autonomously, turn by turn, while the app is open — until its completion \
                contract is satisfied, a real obstacle blocks it, or its budget runs out. The \
                goal survives a session reload, but is not pursued while the app is closed. Use \
                it for multi-turn work with a definite result, not for a one-off reply. Every \
                goal needs a completion contract: the outcome, how success is verified (the goal \
                is 'done' only when this passes), and when to give up. Give it a turn budget \
                (and optionally a deadline). Manage goals with show/list/pause/resume/cancel."
                .into(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["create", "show", "list", "pause", "resume", "cancel"],
                        "description": "What to do."
                    },
                    "objective": {
                        "type": "string",
                        "description": "For create: the user's objective, in their terms."
                    },
                    "outcome": {
                        "type": "string",
                        "description": "For create: what 'done' means — the concrete outcome to reach."
                    },
                    "verification": {
                        "type": "string",
                        "description": "For create: how success is verified (a command, a check, an artifact's existence). The goal is done only when this passes."
                    },
                    "stop_condition": {
                        "type": "string",
                        "description": "For create: when to give up rather than keep trying."
                    },
                    "constraints": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "For create, optional: things that must hold throughout."
                    },
                    "boundaries": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "For create, optional: hard limits never to cross (e.g. 'never push, only commit locally')."
                    },
                    "subgoals": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "For create, optional: a starting checklist of steps."
                    },
                    "max_turns": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "For create: the maximum number of autonomous turns to spend."
                    },
                    "deadline": {
                        "type": "string",
                        "description": "For create, optional: a wall-clock deadline, YYYY-MM-DD HH:MM; past it the goal fails."
                    },
                    "id": {
                        "type": "string",
                        "description": "For show/pause/resume/cancel: the goal id."
                    }
                },
                "required": ["action"]
            }),
            annotations: None,
            capabilities: ToolSpec::capabilities(&[
                capabilities::SCOPE_AGENT,
                capabilities::SCOPE_AGENT_DIFF,
            ]),
            multiline_params: &["objective", "outcome", "verification", "stop_condition"],
            hidden: false,
            title_template: Some("Managing goal ({action})"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        let session_id = context
            .session_id
            .clone()
            .ok_or_else(|| anyhow!("no session id in this context; goals are unavailable here"))?;
        match input.action {
            GoalAction::Create => self.create(&session_id, input),
            GoalAction::Show => self.show(&session_id, input),
            GoalAction::List => self.list(&session_id),
            GoalAction::Pause => self.pause(&session_id, input),
            GoalAction::Resume => self.resume(&session_id, input),
            GoalAction::Cancel => self.cancel(&session_id, input),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 7, 16)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
    }

    fn tool(dir: &std::path::Path) -> GoalTool {
        GoalTool::new(dir.join("goals.json")).with_now(now())
    }

    async fn run(tool: &GoalTool, session_id: &str, input: GoalInput) -> Result<GoalOutput> {
        let executor = crate::mocks::create_command_executor_mock();
        let mut context = ToolContext {
            command_executor: &executor,
            tool_id: None,
            session_id: Some(session_id.to_string()),
            permission_handler: None,
            extensions: None,
        };
        let mut input = input;
        tool.execute(&mut context, &mut input).await
    }

    fn create_input(objective: &str) -> GoalInput {
        GoalInput {
            action: GoalAction::Create,
            objective: Some(objective.to_string()),
            outcome: Some("it is done".to_string()),
            verification: Some("the check passes".to_string()),
            stop_condition: Some("three failed strategies".to_string()),
            constraints: None,
            boundaries: None,
            subgoals: Some(vec!["step one".to_string()]),
            max_turns: Some(5),
            deadline: None,
            id: None,
        }
    }

    fn action_on(action: GoalAction, id: &str) -> GoalInput {
        GoalInput {
            action,
            objective: None,
            outcome: None,
            verification: None,
            stop_condition: None,
            constraints: None,
            boundaries: None,
            subgoals: None,
            max_turns: None,
            deadline: None,
            id: Some(id.to_string()),
        }
    }

    #[tokio::test]
    async fn create_persists_a_session_owned_running_goal() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool(dir.path());

        let output = run(&tool, "sess-1", create_input("ship it")).await.unwrap();
        assert!(output.success);

        let goals = tool.store().list().unwrap();
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].owner, session_owner("sess-1"));
        assert_eq!(goals[0].state, GoalState::Running);
        assert_eq!(goals[0].subgoals.len(), 1);
        assert!(output.message.contains(&goals[0].id));
    }

    #[tokio::test]
    async fn create_validates_the_contract_fields() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool(dir.path());
        let mut input = create_input("ship it");
        input.verification = None;

        let error = run(&tool, "sess-1", input).await.unwrap_err();
        assert!(error.to_string().contains("verification"));
        assert!(tool.store().list().unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_and_show_are_scoped_to_the_calling_session() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool(dir.path());
        run(&tool, "sess-1", create_input("mine")).await.unwrap();
        run(&tool, "sess-2", create_input("theirs")).await.unwrap();

        let listed = run(
            &tool,
            "sess-1",
            GoalInput {
                action: GoalAction::List,
                ..create_input("")
            },
        )
        .await
        .unwrap();
        assert!(listed.message.contains("mine"));
        assert!(!listed.message.contains("theirs"));

        // Another session's goal is not visible, steerable, or cancellable.
        let foreign = tool.store().list().unwrap();
        let foreign_id = &foreign
            .iter()
            .find(|g| g.owner == session_owner("sess-2"))
            .unwrap()
            .id;
        let error = run(&tool, "sess-1", action_on(GoalAction::Show, foreign_id))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("another session"));
        let error = run(&tool, "sess-1", action_on(GoalAction::Cancel, foreign_id))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("another session"));
    }

    #[tokio::test]
    async fn show_renders_the_contract_and_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool(dir.path());
        run(&tool, "sess-1", create_input("ship it")).await.unwrap();
        let id = tool.store().list().unwrap()[0].id.clone();

        let shown = run(&tool, "sess-1", action_on(GoalAction::Show, &id))
            .await
            .unwrap();
        assert!(shown.message.contains("Done when: it is done"));
        assert!(shown.message.contains("Verify by: the check passes"));
        assert!(shown.message.contains("0/5 turns used"));
        assert!(shown.message.contains("[ ] step one"));
    }

    #[tokio::test]
    async fn pause_resume_cancel_walk_the_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool(dir.path());
        run(&tool, "sess-1", create_input("ship it")).await.unwrap();
        let id = tool.store().list().unwrap()[0].id.clone();

        run(&tool, "sess-1", action_on(GoalAction::Pause, &id))
            .await
            .unwrap();
        assert_eq!(
            tool.store().get(&id).unwrap().unwrap().state,
            GoalState::Paused
        );

        run(&tool, "sess-1", action_on(GoalAction::Resume, &id))
            .await
            .unwrap();
        assert_eq!(
            tool.store().get(&id).unwrap().unwrap().state,
            GoalState::Running
        );

        run(&tool, "sess-1", action_on(GoalAction::Cancel, &id))
            .await
            .unwrap();
        let goal = tool.store().get(&id).unwrap().unwrap();
        assert_eq!(goal.state, GoalState::Failed);
        assert_eq!(goal.note.as_deref(), Some("cancelled by the user"));

        // Terminal goals cannot change any more.
        let error = run(&tool, "sess-1", action_on(GoalAction::Resume, &id))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("already failed"));
    }

    #[tokio::test]
    async fn pause_during_an_in_flight_turn_does_not_clobber_the_claim() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool(dir.path());
        run(&tool, "sess-1", create_input("ship it")).await.unwrap();
        let store = tool.store();
        let snapshot = store.list().unwrap().remove(0);
        let claim = store.claim_attempt(&snapshot, now()).unwrap().unwrap();

        // The user pauses while the controller's turn is in flight.
        run(&tool, "sess-1", action_on(GoalAction::Pause, &claim.id))
            .await
            .unwrap();

        // The pause advanced the revision but preserved the claim token, so
        // the completed turn still merges into the ledger (as Preempted).
        let paused = store.get(&claim.id).unwrap().unwrap();
        assert_eq!(paused.state, GoalState::Paused);
        assert!(paused.in_flight.is_some(), "pause must not erase the claim");
        use agent_orchestration::goals::{
            AttemptCompletion, AttemptVerdict, ControllerDecision, Evaluation,
        };
        let (merged, decision) = store
            .finish_attempt(
                &claim,
                AttemptCompletion::Evaluated(Evaluation::new(
                    AttemptVerdict::Progressed,
                    "made progress",
                )),
                now(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(decision, ControllerDecision::Preempted);
        assert_eq!(merged.state, GoalState::Paused, "user stop wins");
        assert_eq!(merged.attempts.len(), 1, "the turn still spent its budget");
    }
}
