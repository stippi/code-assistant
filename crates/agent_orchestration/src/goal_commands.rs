//! The `/goal` command language — the only way a goal is created or steered.
//!
//! Goals are deliberately **user-set**: there is no model-facing goal tool,
//! so the agent cannot commit a session to a goal (or steer one) on its own.
//! The user states the condition in their own words; the host's goal
//! controller drives the session against it and the LLM judge decides when
//! the condition holds. This mirrors hermes' `/goal` and Claude Code's
//! stop-hook goals, and keeps the agent's own plan/checklist tool free of
//! competition from a second self-management surface.
//!
//! Frontends pass the raw text after `/goal` to [`GoalCommand::parse`] and
//! display whatever [`run_goal_command_on`] returns — parsing and store
//! access live here so every host gets the same behavior over its own
//! [`GoalRepository`] and owner key (a code-assistant session, a PAL lane).

use crate::goals::{Budget, CompletionContract, Goal, GoalRepository};
use crate::OwnerKey;
use anyhow::{bail, Result};
use chrono::NaiveDateTime;

/// Turn budget for user-committed goals. An honest bound: the controller
/// stops burning turns on a goal that is not converging, instead of looping
/// until the user notices.
pub const USER_GOAL_MAX_TURNS: u32 = 10;

/// The complete user-facing syntax. Frontends show this when `/goal` is
/// submitted without the required objective.
pub const GOAL_USAGE: &str =
    "Usage: /goal <completion criteria> or /goal status | pause | resume | cancel";

/// A parsed `/goal` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalCommand {
    /// `/goal cancel`: delete the owner's current goal.
    Cancel,
    /// `/goal status`: show the current goal's state and progress.
    Status,
    /// `/goal pause`: stop the controller from driving the goal.
    Pause,
    /// `/goal resume`: let the controller drive a paused/blocked goal again.
    Resume,
    /// `/goal <completion criteria>`: set or replace the owner's one goal.
    Set { objective: String },
}

impl GoalCommand {
    /// Parse the raw text after `/goal`. Only the exact single words
    /// `cancel`, `status`, `pause` and `resume` are control syntax; every
    /// other non-empty string is the user's objective verbatim. A bare
    /// `/goal` is deliberately invalid, so UIs can keep the user in the
    /// `/goal <completion criteria>` template.
    pub fn parse(input: &str) -> Result<GoalCommand> {
        let input = input.trim();
        if input.is_empty() {
            bail!(GOAL_USAGE);
        }
        if input.eq_ignore_ascii_case("cancel") {
            Ok(GoalCommand::Cancel)
        } else if input.eq_ignore_ascii_case("status") {
            Ok(GoalCommand::Status)
        } else if input.eq_ignore_ascii_case("pause") {
            Ok(GoalCommand::Pause)
        } else if input.eq_ignore_ascii_case("resume") {
            Ok(GoalCommand::Resume)
        } else {
            Ok(GoalCommand::Set {
                objective: input.to_string(),
            })
        }
    }
}

/// Execute a `/goal` command for `owner` against any [`GoalRepository`].
/// Returns the text the frontend should show.
pub fn run_goal_command_on(
    repo: &dyn GoalRepository,
    owner: OwnerKey,
    command: &GoalCommand,
    now: NaiveDateTime,
) -> Result<String> {
    match command {
        GoalCommand::Cancel => cancel(repo, &owner),
        GoalCommand::Status => status(repo, &owner),
        GoalCommand::Pause => pause(repo, &owner, now),
        GoalCommand::Resume => resume(repo, &owner, now),
        GoalCommand::Set { objective } => set(repo, owner, objective, now),
    }
}

pub const NO_GOAL: &str = "No goal is set.";

fn set(
    repo: &dyn GoalRepository,
    owner: OwnerKey,
    objective: &str,
    now: NaiveDateTime,
) -> Result<String> {
    // The user's condition is the whole contract: it is both the objective
    // and the outcome, verified against the session's own turn evidence.
    let contract = CompletionContract::new(
        objective,
        "evidence from the session's turns (commands run and their results, artifacts \
         produced) shows the condition holds",
        "the user replaces or cancels the goal",
    );
    let (goal, replaced) = repo.replace_current_for_owner(
        owner,
        objective.to_string(),
        contract,
        Budget::turns(USER_GOAL_MAX_TURNS),
        now,
    )?;
    let verb = if replaced == 0 { "set" } else { "replaced" };
    Ok(format!(
        "Goal {verb}: {}\nThe controller pursues it (up to {} turns). \
         Use /goal cancel to remove it.",
        goal.objective, goal.budget.max_turns,
    ))
}

fn cancel(repo: &dyn GoalRepository, owner: &OwnerKey) -> Result<String> {
    let removed = repo.remove_current_for_owner(owner)?;
    if removed == 0 {
        Ok(NO_GOAL.into())
    } else {
        Ok("Goal cancelled and removed.".into())
    }
}

fn status(repo: &dyn GoalRepository, owner: &OwnerKey) -> Result<String> {
    let goals = repo.active_for_owner(owner)?;
    match current(goals) {
        None => Ok(NO_GOAL.into()),
        Some(goal) => {
            let mut lines = vec![
                format!("Goal ({}): {}", goal.state.label(), goal.objective),
                format!(
                    "Turns used: {}/{}",
                    goal.turns_used(),
                    goal.budget.max_turns
                ),
            ];
            if let Some(note) = &goal.note {
                lines.push(format!("Note: {note}"));
            }
            Ok(lines.join("\n"))
        }
    }
}

fn pause(repo: &dyn GoalRepository, owner: &OwnerKey, now: NaiveDateTime) -> Result<String> {
    steer(
        repo,
        owner,
        "Goal paused. Use /goal resume to continue.",
        |g| g.pause(now),
    )
}

fn resume(repo: &dyn GoalRepository, owner: &OwnerKey, now: NaiveDateTime) -> Result<String> {
    steer(
        repo,
        owner,
        "Goal resumed; the controller drives it again.",
        |g| g.resume(now),
    )
}

/// Apply a lifecycle edge to the owner's current goal and persist it. An
/// illegal edge (e.g. resuming a goal that is not paused) surfaces the
/// domain's own error text.
fn steer(
    repo: &dyn GoalRepository,
    owner: &OwnerKey,
    done: &str,
    edge: impl FnOnce(&mut Goal) -> Result<()>,
) -> Result<String> {
    let goals = repo.active_for_owner(owner)?;
    match current(goals) {
        None => Ok(NO_GOAL.into()),
        Some(mut goal) => {
            edge(&mut goal)?;
            repo.update(&goal)?;
            Ok(format!(
                "{done}\nGoal ({}): {}",
                goal.state.label(),
                goal.objective
            ))
        }
    }
}

/// The owner's single current goal. The `/goal` surface maintains at most one
/// non-terminal goal per owner (`replace_current_for_owner`); if older state
/// holds several, steer the most recently updated one.
fn current(mut goals: Vec<Goal>) -> Option<Goal> {
    goals.sort_by_key(|g| g.updated_at);
    goals.pop()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goals::{GoalState, GoalStore};
    use chrono::NaiveDate;

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 7, 16)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
    }

    fn owner(name: &str) -> OwnerKey {
        OwnerKey::from_parts(&["session", name])
    }

    fn run(dir: &std::path::Path, who: &str, input: &str) -> Result<String> {
        let command = GoalCommand::parse(input)?;
        let store = GoalStore::new(dir.join("goals.json"));
        run_goal_command_on(&store, owner(who), &command, now())
    }

    fn store(dir: &std::path::Path) -> GoalStore {
        GoalStore::new(dir.join("goals.json"))
    }

    #[test]
    fn parse_enforces_the_small_user_owned_command_language() {
        assert_eq!(GoalCommand::parse("").unwrap_err().to_string(), GOAL_USAGE);
        assert_eq!(GoalCommand::parse(" CANCEL ").unwrap(), GoalCommand::Cancel);
        assert_eq!(GoalCommand::parse("status").unwrap(), GoalCommand::Status);
        assert_eq!(GoalCommand::parse("pause").unwrap(), GoalCommand::Pause);
        assert_eq!(GoalCommand::parse("Resume").unwrap(), GoalCommand::Resume);
        assert_eq!(
            GoalCommand::parse("all tests pass").unwrap(),
            GoalCommand::Set {
                objective: "all tests pass".into()
            }
        );
        // Only an exact keyword is control syntax; these remain objectives.
        assert_eq!(
            GoalCommand::parse("cancel the subscription").unwrap(),
            GoalCommand::Set {
                objective: "cancel the subscription".into()
            }
        );
        assert_eq!(
            GoalCommand::parse("pause the deployment pipeline").unwrap(),
            GoalCommand::Set {
                objective: "pause the deployment pipeline".into()
            }
        );
    }

    #[test]
    fn set_persists_one_owner_goal_with_the_objective_as_contract() {
        let dir = tempfile::tempdir().unwrap();
        let message = run(dir.path(), "sess-1", "the CI badge is green").unwrap();

        let goals = store(dir.path()).list().unwrap();
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].owner, owner("sess-1"));
        assert_eq!(goals[0].state, GoalState::Running);
        assert_eq!(goals[0].objective, "the CI badge is green");
        assert_eq!(goals[0].contract.outcome, "the CI badge is green");
        assert_eq!(goals[0].budget.max_turns, USER_GOAL_MAX_TURNS);
        assert!(message.contains("Goal set"));
    }

    #[test]
    fn setting_again_replaces_only_the_current_owners_goal() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), "sess-1", "old objective").unwrap();
        run(dir.path(), "sess-2", "theirs holds").unwrap();
        let message = run(dir.path(), "sess-1", "new objective").unwrap();

        let goals = store(dir.path()).list().unwrap();
        assert_eq!(goals.len(), 2);
        assert!(goals
            .iter()
            .any(|goal| goal.owner == owner("sess-1") && goal.objective == "new objective"));
        assert!(goals
            .iter()
            .any(|goal| goal.owner == owner("sess-2") && goal.objective == "theirs holds"));
        assert!(!goals.iter().any(|goal| goal.objective == "old objective"));
        assert!(message.contains("Goal replaced"));
    }

    #[test]
    fn cancel_deletes_the_current_owners_goal() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), "sess-1", "mine").unwrap();
        run(dir.path(), "sess-2", "theirs").unwrap();

        let message = run(dir.path(), "sess-1", "cancel").unwrap();
        let goals = store(dir.path()).list().unwrap();
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].owner, owner("sess-2"));
        assert_eq!(goals[0].state, GoalState::Running);
        assert_eq!(message, "Goal cancelled and removed.");

        assert_eq!(run(dir.path(), "sess-1", "cancel").unwrap(), NO_GOAL);
    }

    #[test]
    fn status_reports_state_objective_and_budget() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(run(dir.path(), "sess-1", "status").unwrap(), NO_GOAL);

        run(dir.path(), "sess-1", "the CI badge is green").unwrap();
        let message = run(dir.path(), "sess-1", "status").unwrap();
        assert!(message.contains("Goal (running): the CI badge is green"));
        assert!(message.contains("Turns used: 0/10"));
    }

    #[test]
    fn pause_and_resume_steer_the_current_goal() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(run(dir.path(), "sess-1", "pause").unwrap(), NO_GOAL);

        run(dir.path(), "sess-1", "objective").unwrap();
        let message = run(dir.path(), "sess-1", "pause").unwrap();
        assert!(message.contains("Goal paused"));
        assert_eq!(
            store(dir.path()).list().unwrap()[0].state,
            GoalState::Paused
        );
        // The controller skips paused goals; status still shows it.
        assert!(run(dir.path(), "sess-1", "status")
            .unwrap()
            .contains("Goal (paused)"));

        let message = run(dir.path(), "sess-1", "resume").unwrap();
        assert!(message.contains("Goal resumed"));
        assert_eq!(
            store(dir.path()).list().unwrap()[0].state,
            GoalState::Running
        );
    }

    #[test]
    fn resuming_a_running_goal_is_idempotent_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), "sess-1", "objective").unwrap();
        // Re-entering the same state is a legal (idempotent) edge in the
        // state machine, so a double resume stays friendly.
        let message = run(dir.path(), "sess-1", "resume").unwrap();
        assert!(message.contains("Goal (running)"));
    }
}
