//! Running sub-agents in the terminal: the status lines above the composer
//! and the `/agents` command.

use code_assistant_core::agent::sub_agent::{RunningSubAgentInfo, SubAgentActivity};

/// What `/agents` asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentsCommand {
    /// List the running sub-agents.
    List,
    /// Cancel one sub-agent by its `#id`, or all of them (`None`).
    Cancel(Option<u64>),
    /// Show the transcript of sub-agent `#id` (read-only).
    Open(u64),
    /// Leave a transcript for the session that ran the sub-agent.
    Back,
}

impl AgentsCommand {
    /// Parse the arguments after `/agents`.
    pub fn parse(args: &[&str]) -> Result<Self, String> {
        match args {
            [] | ["list"] => Ok(Self::List),
            ["cancel", "all"] => Ok(Self::Cancel(None)),
            ["cancel", id] => parse_id(id).map(|id| Self::Cancel(Some(id))),
            ["open", id] => parse_id(id).map(Self::Open),
            ["back"] => Ok(Self::Back),
            _ => Err("Usage: /agents [cancel <id>|cancel all|open <id>|back]".to_string()),
        }
    }

    /// The session `/agents open <id>` or `/agents back` switches to from
    /// `current`, or what is wrong.
    pub fn target_session(&self, current: &str) -> Result<String, String> {
        use code_assistant_core::persistence::layout::{sub_agent_parent, sub_agent_session_id};
        // A transcript's sub-agents are its parent's.
        let parent = sub_agent_parent(current).map(|(parent, _)| parent);
        match self {
            Self::Open(id) => Ok(sub_agent_session_id(parent.unwrap_or(current), *id)),
            Self::Back => parent
                .map(str::to_string)
                .ok_or_else(|| "This is not a sub-agent's transcript".to_string()),
            _ => Err("Not a navigation command".to_string()),
        }
    }

    /// The tool ids of the sub-agents a cancel targets, or what is wrong.
    pub fn cancel_targets(
        target: Option<u64>,
        running: &[RunningSubAgentInfo],
    ) -> Result<Vec<String>, String> {
        let targets: Vec<String> = running
            .iter()
            .filter(|agent| target.is_none_or(|id| agent.id == id))
            .map(|agent| agent.tool_id.clone())
            .collect();
        match (target, targets.is_empty()) {
            (_, false) => Ok(targets),
            (Some(id), true) => Err(format!("No sub-agent #{id} is running")),
            (None, true) => Err("No sub-agents are running".to_string()),
        }
    }
}

fn parse_id(id: &str) -> Result<u64, String> {
    id.trim_start_matches('#')
        .parse()
        .map_err(|_| format!("Not a sub-agent id: {id}"))
}

/// What a running sub-agent is doing, in a few words.
fn activity_text(agent: &RunningSubAgentInfo) -> String {
    if let Some(tool) = &agent.current_tool {
        return tool.clone();
    }
    match agent.activity {
        Some(SubAgentActivity::Streaming) => "responding".into(),
        Some(SubAgentActivity::ExecutingTools) => "running tools".into(),
        _ => "thinking".into(),
    }
}

/// One line per running sub-agent, as markdown.
pub fn running_lines(agents: &[RunningSubAgentInfo]) -> Option<String> {
    if agents.is_empty() {
        return None;
    }
    let lines: Vec<String> = agents
        .iter()
        .map(|agent| {
            let background = if agent.background {
                " (background)"
            } else {
                ""
            };
            format!(
                "↻ #{} {}{background} · {}",
                agent.id,
                agent.title,
                activity_text(agent)
            )
        })
        .collect();
    Some(lines.join("  \n"))
}

/// The `/agents` listing.
pub fn listing(agents: &[RunningSubAgentInfo]) -> String {
    match running_lines(agents) {
        Some(lines) => {
            format!(
                "Running sub-agents:  \n{lines}  \nOpen one with /agents open <id>, cancel it with /agents cancel <id>."
            )
        }
        None => "No sub-agents are running.".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: u64, background: bool) -> RunningSubAgentInfo {
        RunningSubAgentInfo {
            id,
            tool_id: format!("tool-{id}"),
            title: format!("Task {id}"),
            background,
            started_at: chrono::Utc::now(),
            activity: Some(SubAgentActivity::ExecutingTools),
            tool_count: 1,
            current_tool: (id == 2).then(|| "Reading main.rs".to_string()),
            session_id: None,
        }
    }

    #[test]
    fn parses_the_agents_command() {
        assert_eq!(AgentsCommand::parse(&[]), Ok(AgentsCommand::List));
        assert_eq!(AgentsCommand::parse(&["list"]), Ok(AgentsCommand::List));
        assert_eq!(
            AgentsCommand::parse(&["cancel", "#3"]),
            Ok(AgentsCommand::Cancel(Some(3)))
        );
        assert_eq!(
            AgentsCommand::parse(&["cancel", "all"]),
            Ok(AgentsCommand::Cancel(None))
        );
        assert_eq!(
            AgentsCommand::parse(&["open", "2"]),
            Ok(AgentsCommand::Open(2))
        );
        assert_eq!(AgentsCommand::parse(&["back"]), Ok(AgentsCommand::Back));
        assert!(AgentsCommand::parse(&["cancel", "x"]).is_err());
        assert!(AgentsCommand::parse(&["stop"]).is_err());
    }

    #[test]
    fn navigates_between_a_session_and_its_transcripts() {
        let open = AgentsCommand::Open(2);
        assert_eq!(
            open.target_session("p/1"),
            Ok("p/1/sub-agents/2".to_string())
        );
        // From one transcript to a sibling.
        assert_eq!(
            open.target_session("p/1/sub-agents/1"),
            Ok("p/1/sub-agents/2".to_string())
        );
        assert_eq!(
            AgentsCommand::Back.target_session("p/1/sub-agents/1"),
            Ok("p/1".to_string())
        );
        assert!(AgentsCommand::Back.target_session("p/1").is_err());
    }

    #[test]
    fn cancel_targets_running_agents_only() {
        let running = [agent(1, true), agent(2, false)];
        assert_eq!(
            AgentsCommand::cancel_targets(Some(2), &running),
            Ok(vec!["tool-2".to_string()])
        );
        assert_eq!(
            AgentsCommand::cancel_targets(None, &running).unwrap().len(),
            2
        );
        assert!(AgentsCommand::cancel_targets(Some(9), &running).is_err());
        assert!(AgentsCommand::cancel_targets(None, &[]).is_err());
    }

    #[test]
    fn lists_what_each_agent_does() {
        let lines = running_lines(&[agent(1, true), agent(2, false)]).unwrap();
        assert_eq!(
            lines,
            "↻ #1 Task 1 (background) · running tools  \n↻ #2 Task 2 · Reading main.rs"
        );
        assert!(running_lines(&[]).is_none());
        assert_eq!(listing(&[]), "No sub-agents are running.");
    }
}
