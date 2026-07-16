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
use agent_orchestration::goals::{Budget, CompletionContract, Goal, GoalState, GoalStore};
use anyhow::{anyhow, Result};
use chrono::NaiveDateTime;
use std::path::Path;

/// Turn budget for user-committed goals. An honest bound: the controller
/// stops burning turns on a goal that is not converging, instead of looping
/// until the user notices.
pub const USER_GOAL_MAX_TURNS: u32 = 10;

/// A parsed `/goal` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalCommand {
    /// Bare `/goal` (or `/goal list`): the session's goals and their state.
    List,
    /// `/goal show [id]`: one goal in full detail.
    Show { id: Option<String> },
    /// `/goal pause [id]`: the controller stops driving the goal.
    Pause { id: Option<String> },
    /// `/goal resume [id]`: the controller drives the goal again.
    Resume { id: Option<String> },
    /// `/goal cancel [id]`: give up; the goal stays on the ledger as failed.
    Cancel { id: Option<String> },
    /// `/goal <condition>`: commit the session to a new goal whose contract
    /// is the user's condition, verbatim.
    Commit { condition: String },
}

impl GoalCommand {
    /// Parse the raw text after `/goal`. A leading lifecycle keyword with at
    /// most one trailing token is a lifecycle command; everything else is the
    /// condition of a new goal (so prose that merely starts with "cancel …"
    /// still commits a goal).
    pub fn parse(input: &str) -> GoalCommand {
        let input = input.trim();
        let tokens: Vec<&str> = input.split_whitespace().collect();
        let id = (tokens.len() == 2).then(|| tokens[1].to_string());
        match (tokens.first().map(|t| t.to_lowercase()), tokens.len()) {
            (None, _) => GoalCommand::List,
            (Some(word), 1) => match word.as_str() {
                "list" | "status" => GoalCommand::List,
                "show" => GoalCommand::Show { id: None },
                "pause" => GoalCommand::Pause { id: None },
                "resume" => GoalCommand::Resume { id: None },
                "cancel" => GoalCommand::Cancel { id: None },
                _ => GoalCommand::Commit {
                    condition: input.to_string(),
                },
            },
            (Some(word), 2) => match word.as_str() {
                "show" => GoalCommand::Show { id },
                "pause" => GoalCommand::Pause { id },
                "resume" => GoalCommand::Resume { id },
                "cancel" => GoalCommand::Cancel { id },
                _ => GoalCommand::Commit {
                    condition: input.to_string(),
                },
            },
            _ => GoalCommand::Commit {
                condition: input.to_string(),
            },
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
/// `goals_path`. Returns the text the frontend should show; errors are
/// user-facing too (unknown id, ambiguous target, …).
pub fn run_goal_command(
    goals_path: &Path,
    session_id: &str,
    command: &GoalCommand,
    now: NaiveDateTime,
) -> Result<String> {
    let store = GoalStore::new(goals_path);
    match command {
        GoalCommand::List => list(&store, session_id),
        GoalCommand::Show { id } => show(&store, session_id, id.as_deref()),
        GoalCommand::Pause { id } => pause(&store, session_id, id.as_deref(), now),
        GoalCommand::Resume { id } => resume(&store, session_id, id.as_deref(), now),
        GoalCommand::Cancel { id } => cancel(&store, session_id, id.as_deref(), now),
        GoalCommand::Commit { condition } => commit(&store, session_id, condition, now),
    }
}

fn commit(
    store: &GoalStore,
    session_id: &str,
    condition: &str,
    now: NaiveDateTime,
) -> Result<String> {
    let condition = condition.trim();
    if condition.is_empty() {
        return Err(anyhow!("the goal needs a condition: /goal <condition>"));
    }
    // The user's condition is the whole contract: it is both the objective
    // and the outcome, verified against the session's own turn evidence.
    let contract = CompletionContract::new(
        condition,
        "evidence from the session's turns (commands run and their results, artifacts \
         produced) shows the condition holds",
        "the user pauses or cancels the goal",
    );
    let goal = store.add_new(
        session_owner(session_id),
        condition,
        contract,
        Budget::turns(USER_GOAL_MAX_TURNS),
        now,
    )?;
    Ok(format!(
        "Goal {} set: {}\nThe controller pursues it while the app is open (up to {} turns); \
         /goal shows progress, /goal cancel gives it up.",
        goal.id, condition, goal.budget.max_turns,
    ))
}

fn list(store: &GoalStore, session_id: &str) -> Result<String> {
    let mut goals = session_goals(store, session_id)?;
    if goals.is_empty() {
        return Ok("No goals on record for this session. Set one with /goal <condition>.".into());
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
    Ok(lines.join("\n"))
}

fn show(store: &GoalStore, session_id: &str, id: Option<&str>) -> Result<String> {
    let goal = target_goal(store, session_id, id)?;
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
        format!(
            "- Budget: {}/{} turns used",
            goal.turns_used(),
            goal.budget.max_turns,
        ),
    ];
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
    Ok(lines.join("\n"))
}

fn pause(
    store: &GoalStore,
    session_id: &str,
    id: Option<&str>,
    now: NaiveDateTime,
) -> Result<String> {
    let mut goal = target_goal(store, session_id, id)?;
    if goal.state == GoalState::Paused {
        return Ok(format!("Goal {} is already paused.", goal.id));
    }
    goal.pause(now)
        .map_err(|_| finished_error(&goal.id, goal.state))?;
    store.update(&goal)?;
    Ok(format!(
        "Paused goal {}. /goal resume drives it again.",
        goal.id
    ))
}

fn resume(
    store: &GoalStore,
    session_id: &str,
    id: Option<&str>,
    now: NaiveDateTime,
) -> Result<String> {
    let mut goal = target_goal(store, session_id, id)?;
    if goal.state == GoalState::Running {
        return Ok(format!("Goal {} is already running.", goal.id));
    }
    goal.resume(now)
        .map_err(|_| finished_error(&goal.id, goal.state))?;
    store.update(&goal)?;
    Ok(format!(
        "Resumed goal {}; the controller drives it on the next pass.",
        goal.id
    ))
}

fn cancel(
    store: &GoalStore,
    session_id: &str,
    id: Option<&str>,
    now: NaiveDateTime,
) -> Result<String> {
    let mut goal = target_goal(store, session_id, id)?;
    goal.fail("cancelled by the user", now)
        .map_err(|_| finished_error(&goal.id, goal.state))?;
    store.update(&goal)?;
    Ok(format!(
        "Cancelled goal {}; it stays on the ledger as failed.",
        goal.id
    ))
}

fn session_goals(store: &GoalStore, session_id: &str) -> Result<Vec<Goal>> {
    let owner = session_owner(session_id);
    Ok(store
        .list()?
        .into_iter()
        .filter(|g| g.owner == owner)
        .collect())
}

/// Resolve the goal a lifecycle command targets: an explicit id (which must
/// belong to this session), or — when omitted — the session's single
/// non-terminal goal.
fn target_goal(store: &GoalStore, session_id: &str, id: Option<&str>) -> Result<Goal> {
    if let Some(id) = id {
        let goal = store
            .get(id)?
            .ok_or_else(|| anyhow!("no goal with id {id}"))?;
        if goal.owner != session_owner(session_id) {
            return Err(anyhow!("goal {id} belongs to another session"));
        }
        return Ok(goal);
    }
    let mut open: Vec<Goal> = session_goals(store, session_id)?
        .into_iter()
        .filter(|g| !g.state.is_terminal())
        .collect();
    match open.len() {
        0 => Err(anyhow!("this session has no open goal")),
        1 => Ok(open.remove(0)),
        _ => Err(anyhow!(
            "this session has {} open goals — name one: {}",
            open.len(),
            open.iter()
                .map(|g| g.id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        )),
    }
}

fn finished_error(id: &str, state: GoalState) -> anyhow::Error {
    anyhow!("goal {id} is already {} and cannot change", state.label())
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

    fn run(dir: &std::path::Path, session_id: &str, input: &str) -> Result<String> {
        run_goal_command(
            &dir.join("goals.json"),
            session_id,
            &GoalCommand::parse(input),
            now(),
        )
    }

    fn store(dir: &std::path::Path) -> GoalStore {
        GoalStore::new(dir.join("goals.json"))
    }

    #[test]
    fn parse_separates_lifecycle_keywords_from_conditions() {
        assert_eq!(GoalCommand::parse(""), GoalCommand::List);
        assert_eq!(GoalCommand::parse("  list "), GoalCommand::List);
        assert_eq!(GoalCommand::parse("status"), GoalCommand::List);
        assert_eq!(GoalCommand::parse("show"), GoalCommand::Show { id: None });
        assert_eq!(
            GoalCommand::parse("pause g-1"),
            GoalCommand::Pause {
                id: Some("g-1".into())
            }
        );
        assert_eq!(
            GoalCommand::parse("all tests pass"),
            GoalCommand::Commit {
                condition: "all tests pass".into()
            }
        );
        // A three-word phrase starting with a keyword is a condition, not a
        // lifecycle command with a two-token id.
        assert_eq!(
            GoalCommand::parse("cancel the subscription in the billing portal"),
            GoalCommand::Commit {
                condition: "cancel the subscription in the billing portal".into()
            }
        );
    }

    #[test]
    fn commit_persists_a_session_owned_running_goal_with_the_condition_as_contract() {
        let dir = tempfile::tempdir().unwrap();
        let message = run(dir.path(), "sess-1", "the CI badge is green").unwrap();

        let goals = store(dir.path()).list().unwrap();
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].owner, session_owner("sess-1"));
        assert_eq!(goals[0].state, GoalState::Running);
        assert_eq!(goals[0].objective, "the CI badge is green");
        assert_eq!(goals[0].contract.outcome, "the CI badge is green");
        assert_eq!(goals[0].budget.max_turns, USER_GOAL_MAX_TURNS);
        assert!(message.contains(&goals[0].id));
    }

    #[test]
    fn list_and_lifecycle_are_scoped_to_the_session() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), "sess-1", "mine holds").unwrap();
        run(dir.path(), "sess-2", "theirs holds").unwrap();

        let listed = run(dir.path(), "sess-1", "").unwrap();
        assert!(listed.contains("mine holds"));
        assert!(!listed.contains("theirs holds"));

        let foreign_id = store(dir.path())
            .list()
            .unwrap()
            .into_iter()
            .find(|g| g.owner == session_owner("sess-2"))
            .unwrap()
            .id;
        let error = run(dir.path(), "sess-1", &format!("cancel {foreign_id}")).unwrap_err();
        assert!(error.to_string().contains("another session"));
    }

    #[test]
    fn lifecycle_without_an_id_targets_the_single_open_goal() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), "sess-1", "the widget ships").unwrap();

        run(dir.path(), "sess-1", "pause").unwrap();
        assert_eq!(
            store(dir.path()).list().unwrap()[0].state,
            GoalState::Paused
        );

        run(dir.path(), "sess-1", "resume").unwrap();
        assert_eq!(
            store(dir.path()).list().unwrap()[0].state,
            GoalState::Running
        );

        run(dir.path(), "sess-1", "cancel").unwrap();
        let goal = &store(dir.path()).list().unwrap()[0];
        assert_eq!(goal.state, GoalState::Failed);
        assert_eq!(goal.note.as_deref(), Some("cancelled by the user"));

        // No open goal left: lifecycle commands now need nothing to act on.
        let error = run(dir.path(), "sess-1", "pause").unwrap_err();
        assert!(error.to_string().contains("no open goal"));
    }

    #[test]
    fn lifecycle_without_an_id_refuses_an_ambiguous_target() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), "sess-1", "goal one holds").unwrap();
        run(dir.path(), "sess-1", "goal two holds").unwrap();

        let error = run(dir.path(), "sess-1", "cancel").unwrap_err();
        assert!(error.to_string().contains("2 open goals"));
    }

    #[test]
    fn show_renders_the_contract_and_ledger() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), "sess-1", "the check passes").unwrap();

        let shown = run(dir.path(), "sess-1", "show").unwrap();
        assert!(shown.contains("Done when: the check passes"));
        assert!(shown.contains(&format!("0/{USER_GOAL_MAX_TURNS} turns used")));
    }

    #[test]
    fn pause_during_an_in_flight_turn_does_not_clobber_the_claim() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), "sess-1", "ship it").unwrap();
        let store = store(dir.path());
        let snapshot = store.list().unwrap().remove(0);
        let claim = store.claim_attempt(&snapshot, now()).unwrap().unwrap();

        // The user pauses while the controller's turn is in flight.
        run(dir.path(), "sess-1", &format!("pause {}", claim.id)).unwrap();

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
