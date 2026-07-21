//! The `/goal` command over code-assistant sessions.
//!
//! The command language and execution engine live in
//! [`agent_orchestration::goal_commands`] (shared with other hosts, e.g.
//! PAL's lanes); this module binds them to a session id and the bundled
//! JSON [`GoalStore`](agent_orchestration::goals::GoalStore). Frontends
//! pass the raw text after `/goal` to [`GoalCommand::parse`] and display
//! whatever [`run_goal_command`] returns.

use crate::goals::session_owner;
use agent_orchestration::goals::GoalStore;
use anyhow::Result;
use chrono::NaiveDateTime;
use std::path::Path;

pub use agent_orchestration::goal_commands::{
    run_goal_command_on, GoalCommand, GOAL_USAGE, USER_GOAL_MAX_TURNS,
};

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

/// Execute a `/goal` command for the given session against the JSON store at
/// `goals_path`. Returns the text the frontend should show.
pub fn run_goal_command(
    goals_path: &Path,
    session_id: &str,
    command: &GoalCommand,
    now: NaiveDateTime,
) -> Result<String> {
    let store = GoalStore::new(goals_path);
    run_goal_command_on(&store, session_owner(session_id), command, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_orchestration::goals::GoalState;

    // The command language and store behavior are covered in
    // `agent_orchestration::goal_commands`; this exercises the session
    // binding end to end.
    #[test]
    fn commands_run_against_the_sessions_owner_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("goals.json");
        let now = chrono::NaiveDate::from_ymd_opt(2026, 7, 16)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap();

        let command = GoalCommand::parse("the CI badge is green").unwrap();
        let message = run_goal_command(&path, "sess-1", &command, now).unwrap();
        assert!(message.contains("Goal set"));

        let goals = GoalStore::new(&path).list().unwrap();
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].owner, session_owner("sess-1"));
        assert_eq!(goals[0].state, GoalState::Running);

        let cancel = GoalCommand::parse("cancel").unwrap();
        assert_eq!(
            run_goal_command(&path, "sess-1", &cancel, now).unwrap(),
            "Goal cancelled and removed."
        );
    }
}
