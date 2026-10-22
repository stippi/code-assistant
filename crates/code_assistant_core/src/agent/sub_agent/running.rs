//! The sub-agents currently running in a session, foreground and background,
//! as frontends list them. Lives on the session instance, so background
//! agents stay listed after the turn that started them ended.

use super::{SubAgentActivity, SubAgentOutput, SubAgentToolStatus};
use crate::ui::ui_events::ToolResultData;
use agent_core::SerializedToolExecution;
use std::sync::Mutex;

/// A running sub-agent, as frontends show it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RunningSubAgentInfo {
    /// Short id, numbered per session (`#3`).
    pub id: u64,
    /// The `spawn_agent` call that started it.
    pub tool_id: String,
    /// The first line of its instructions.
    pub title: String,
    /// Started with `background: true`: the parent went on without waiting.
    pub background: bool,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub activity: Option<SubAgentActivity>,
    /// Tool calls made so far.
    pub tool_count: usize,
    /// The title (or name) of the tool running right now, if any.
    pub current_tool: Option<String>,
}

struct Entry {
    info: RunningSubAgentInfo,
    /// The latest live card state of a background agent; snapshots prefer
    /// it over the stored "started" result.
    live_result: Option<ToolResultData>,
}

#[derive(Default)]
pub struct RunningSubAgents {
    entries: Mutex<Vec<Entry>>,
    /// The last id issued.
    last_id: Mutex<u64>,
}

impl RunningSubAgents {
    /// Make sure new ids continue above `used`, the highest id the session's
    /// history already uses ([`highest_sub_agent_id`]).
    pub fn seed_ids(&self, used: u64) {
        let mut last = self.last_id.lock().unwrap();
        *last = (*last).max(used);
    }

    /// List a newly started sub-agent; returns its id.
    pub fn register(&self, tool_id: &str, instructions: &str, background: bool) -> u64 {
        let id = {
            let mut last = self.last_id.lock().unwrap();
            *last += 1;
            *last
        };
        let mut entries = self.entries.lock().unwrap();
        // A retried call with the same id replaces the earlier entry.
        entries.retain(|e| e.info.tool_id != tool_id);
        entries.push(Entry {
            info: RunningSubAgentInfo {
                id,
                tool_id: tool_id.to_string(),
                title: title_from_instructions(instructions),
                background,
                started_at: chrono::Utc::now(),
                activity: Some(SubAgentActivity::WaitingForLlm),
                tool_count: 0,
                current_tool: None,
            },
            live_result: None,
        });
        id
    }

    /// Record progress; returns whether the listed state changed.
    pub fn update(&self, tool_id: &str, output: &SubAgentOutput) -> bool {
        let mut entries = self.entries.lock().unwrap();
        let Some(entry) = entries.iter_mut().find(|e| e.info.tool_id == tool_id) else {
            return false;
        };
        let current_tool = output
            .tools
            .iter()
            .rev()
            .find(|t| t.status == SubAgentToolStatus::Running)
            .map(|t| t.title.clone().unwrap_or_else(|| t.name.clone()));
        let changed = entry.info.activity != output.activity
            || entry.info.tool_count != output.tools.len()
            || entry.info.current_tool != current_tool;
        entry.info.activity = output.activity;
        entry.info.tool_count = output.tools.len();
        entry.info.current_tool = current_tool;
        changed
    }

    /// Keep the latest card state of a background agent for snapshots.
    pub fn set_live_result(&self, tool_id: &str, result: ToolResultData) {
        let mut entries = self.entries.lock().unwrap();
        if let Some(entry) = entries
            .iter_mut()
            .find(|e| e.info.tool_id == tool_id && e.info.background)
        {
            entry.live_result = Some(result);
        }
    }

    /// Remove a sub-agent that ended.
    pub fn remove(&self, tool_id: &str) {
        self.entries
            .lock()
            .unwrap()
            .retain(|e| e.info.tool_id != tool_id);
    }

    /// The running sub-agents, oldest first.
    pub fn list(&self) -> Vec<RunningSubAgentInfo> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.info.clone())
            .collect()
    }

    /// The live card states of the running background agents.
    pub fn live_results(&self) -> Vec<ToolResultData> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| e.live_result.clone())
            .collect()
    }
}

/// The first non-empty line of `instructions`, shortened for a list row.
fn title_from_instructions(instructions: &str) -> String {
    const MAX_CHARS: usize = 80;
    let line = instructions
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("Sub-agent");
    if line.chars().count() > MAX_CHARS {
        let short: String = line.chars().take(MAX_CHARS - 1).collect();
        format!("{}…", short.trim_end())
    } else {
        line.to_string()
    }
}

/// The highest sub-agent id `executions` (a session's history) already use.
pub fn highest_sub_agent_id(executions: &[SerializedToolExecution]) -> u64 {
    executions
        .iter()
        .filter(|exec| exec.tool_name == "spawn_agent")
        .filter_map(|exec| exec.result_json.get("agent_id")?.as_u64())
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sub_agent::SubAgentToolCall;

    #[test]
    fn ids_continue_above_the_history() {
        let running = RunningSubAgents::default();
        running.seed_ids(4);
        assert_eq!(running.register("t1", "Find the parser", false), 5);
        assert_eq!(running.register("t2", "Find the lexer", true), 6);
        let ids: Vec<u64> = running.list().iter().map(|a| a.id).collect();
        assert_eq!(ids, vec![5, 6]);
        running.remove("t1");
        assert_eq!(running.list().len(), 1);
        assert_eq!(running.list()[0].tool_id, "t2");
    }

    #[test]
    fn update_reports_changes_and_the_running_tool() {
        let running = RunningSubAgents::default();
        running.register("t1", "\n  Look around\nmore", false);
        assert_eq!(running.list()[0].title, "Look around");

        let mut output = SubAgentOutput::new();
        assert!(!running.update("t1", &output));
        output.activity = Some(SubAgentActivity::ExecutingTools);
        output.tools.push(SubAgentToolCall {
            name: "read_files".into(),
            status: SubAgentToolStatus::Running,
            title: Some("Reading main.rs".into()),
            message: None,
            parameters: Default::default(),
        });
        assert!(running.update("t1", &output));
        let info = &running.list()[0];
        assert_eq!(info.tool_count, 1);
        assert_eq!(info.current_tool.as_deref(), Some("Reading main.rs"));
        assert!(!running.update("t1", &output));
        assert!(!running.update("unknown", &output));
    }

    #[test]
    fn long_titles_are_shortened() {
        let title = title_from_instructions(&"x".repeat(200));
        assert_eq!(title.chars().count(), 80);
        assert!(title.ends_with('…'));
    }
}
