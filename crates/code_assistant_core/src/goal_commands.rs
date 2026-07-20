//! The `/goal` command — the only way a goal is created or steered.
//!
//! Goals are deliberately **user-set**: there is no model-facing goal tool,
//! so the agent cannot commit the session to a goal (or steer one) on its
//! own. The user states the condition in their own words; the goal
//! controller (see [`crate::goals`]) drives the session against it and the
//! LLM judge decides when the condition holds. This mirrors hermes' `/goal`
//! and Claude Code's stop-hook goals, and keeps `update_plan` (the agent's
//! own checklist) free of competition from a second self-management tool.
//!
//! Frontends pass the raw text after `/goal` to [`GoalCommand::parse`] and
//! display whatever [`run_goal_command`] returns — parsing and store access
//! live here so every frontend gets the same behavior.

use crate::goals::session_owner;
use agent_orchestration::goals::{Budget, CompletionContract, GoalStore};
use anyhow::{bail, Result};
use chrono::NaiveDateTime;
use std::path::Path;

/// Turn budget for user-committed goals. An honest bound: the controller
/// stops burning turns on a goal that is not converging, instead of looping
/// until the user notices.
pub const USER_GOAL_MAX_TURNS: u32 = 10;

/// The complete user-facing syntax. Frontends show this when `/goal` is
/// submitted without the required objective.
pub const GOAL_USAGE: &str = "Usage: /goal <completion criteria> or /goal cancel";

/// A parsed `/goal` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalCommand {
    /// `/goal cancel`: delete the session's current goal.
    Cancel,
    /// `/goal <completion criteria>`: set or replace the session's one goal.
    Set { objective: String },
}

impl GoalCommand {
    /// Parse the raw text after `/goal`. Only the exact single word `cancel`
    /// is control syntax; every other non-empty string is the user's objective
    /// verbatim. A bare `/goal` is deliberately invalid, so UIs can keep the
    /// user in the `/goal <completion criteria>` template.
    pub fn parse(input: &str) -> Result<GoalCommand> {
        let input = input.trim();
        if input.is_empty() {
            bail!(GOAL_USAGE);
        }
        if input.eq_ignore_ascii_case("cancel") {
            Ok(GoalCommand::Cancel)
        } else {
            Ok(GoalCommand::Set {
                objective: input.to_string(),
            })
        }
    }
}

/// [`run_goal_command`] at the local wall clock.
pub fn run_goal_command_now(
    goals_path: &Path,
    session_id: &str,
    command: &GoalCommand,
) -> Result<String> {
    run_goal_command(
        goals_path,
        session_id,
        command,
        chrono::Local::now().naive_local(),
    )
}

/// Execute a `/goal` command for the given session against the store at
/// `goals_path`. Returns the text the frontend should show.
pub fn run_goal_command(
    goals_path: &Path,
    session_id: &str,
    command: &GoalCommand,
    now: NaiveDateTime,
) -> Result<String> {
    let store = GoalStore::new(goals_path);
    match command {
        GoalCommand::Cancel => cancel(&store, session_id),
        GoalCommand::Set { objective } => set(&store, session_id, objective, now),
    }
}

fn set(store: &GoalStore, session_id: &str, objective: &str, now: NaiveDateTime) -> Result<String> {
    // The user's condition is the whole contract: it is both the objective
    // and the outcome, verified against the session's own turn evidence.
    let contract = CompletionContract::new(
        objective,
        "evidence from the session's turns (commands run and their results, artifacts \
         produced) shows the condition holds",
        "the user replaces or cancels the goal",
    );
    let (goal, replaced) = store.replace_current_for_owner(
        session_owner(session_id),
        objective,
        contract,
        Budget::turns(USER_GOAL_MAX_TURNS),
        now,
    )?;
    let verb = if replaced == 0 { "set" } else { "replaced" };
    Ok(format!(
        "Goal {verb}: {}\nThe controller pursues it while the app is open (up to {} turns). \
         Use /goal cancel to remove it.",
        goal.objective, goal.budget.max_turns,
    ))
}

fn cancel(store: &GoalStore, session_id: &str) -> Result<String> {
    let removed = store.remove_current_for_owner(&session_owner(session_id))?;
    if removed == 0 {
        Ok("No goal is set for this session.".into())
    } else {
        Ok("Goal cancelled and removed.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_orchestration::goals::GoalState;
    use chrono::NaiveDate;

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 7, 16)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
    }

    fn run(dir: &std::path::Path, session_id: &str, input: &str) -> Result<String> {
        let command = GoalCommand::parse(input)?;
        run_goal_command(&dir.join("goals.json"), session_id, &command, now())
    }

    fn store(dir: &std::path::Path) -> GoalStore {
        GoalStore::new(dir.join("goals.json"))
    }

    #[test]
    fn parse_enforces_the_small_user_owned_command_language() {
        assert_eq!(GoalCommand::parse("").unwrap_err().to_string(), GOAL_USAGE);
        assert_eq!(GoalCommand::parse(" CANCEL ").unwrap(), GoalCommand::Cancel);
        assert_eq!(
            GoalCommand::parse("all tests pass").unwrap(),
            GoalCommand::Set {
                objective: "all tests pass".into()
            }
        );
        // Only exact `cancel` is control syntax; this remains an objective.
        assert_eq!(
            GoalCommand::parse("cancel the subscription").unwrap(),
            GoalCommand::Set {
                objective: "cancel the subscription".into()
            }
        );
    }

    #[test]
    fn set_persists_one_session_owned_goal_with_the_objective_as_contract() {
        let dir = tempfile::tempdir().unwrap();
        let message = run(dir.path(), "sess-1", "the CI badge is green").unwrap();

        let goals = store(dir.path()).list().unwrap();
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].owner, session_owner("sess-1"));
        assert_eq!(goals[0].state, GoalState::Running);
        assert_eq!(goals[0].objective, "the CI badge is green");
        assert_eq!(goals[0].contract.outcome, "the CI badge is green");
        assert_eq!(goals[0].budget.max_turns, USER_GOAL_MAX_TURNS);
        assert!(message.contains("Goal set"));
    }

    #[test]
    fn setting_again_replaces_only_the_current_sessions_goal() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), "sess-1", "old objective").unwrap();
        run(dir.path(), "sess-2", "theirs holds").unwrap();
        let message = run(dir.path(), "sess-1", "new objective").unwrap();

        let goals = store(dir.path()).list().unwrap();
        assert_eq!(goals.len(), 2);
        assert!(
            goals
                .iter()
                .any(|goal| goal.owner == session_owner("sess-1")
                    && goal.objective == "new objective")
        );
        assert!(goals
            .iter()
            .any(|goal| goal.owner == session_owner("sess-2") && goal.objective == "theirs holds"));
        assert!(!goals.iter().any(|goal| goal.objective == "old objective"));
        assert!(message.contains("Goal replaced"));
    }

    #[test]
    fn cancel_deletes_the_current_sessions_goal() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), "sess-1", "mine").unwrap();
        run(dir.path(), "sess-2", "theirs").unwrap();

        let message = run(dir.path(), "sess-1", "cancel").unwrap();
        let goals = store(dir.path()).list().unwrap();
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].owner, session_owner("sess-2"));
        assert_eq!(goals[0].state, GoalState::Running);
        assert_eq!(message, "Goal cancelled and removed.");

        assert_eq!(
            run(dir.path(), "sess-1", "cancel").unwrap(),
            "No goal is set for this session."
        );
    }
}
