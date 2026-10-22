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
}

impl AgentsCommand {
    /// Parse the arguments after `/agents`.
    pub fn parse(args: &[&str]) -> Result<Self, String> {
        match args {
            [] | ["list"] => Ok(Self::List),
            ["cancel", "all"] => Ok(Self::Cancel(None)),
            ["cancel", id] => id
                .trim_start_matches('#')
                .parse()
                .map(|id| Self::Cancel(Some(id)))
                .map_err(|_| format!("Not a sub-agent id: {id}")),
            _ => Err("Usage: /agents [cancel <id>|cancel all]".to_string()),
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
            format!("Running sub-agents:  \n{lines}  \nCancel one with /agents cancel <id>.")
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
        assert!(AgentsCommand::parse(&["cancel", "x"]).is_err());
        assert!(AgentsCommand::parse(&["stop"]).is_err());
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
