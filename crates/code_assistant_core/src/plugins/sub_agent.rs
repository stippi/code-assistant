//! Dispatch policy for the `spawn_agent` tool: all sub-agents of a turn run
//! concurrently. Where each one works (the parent's tree or a worktree of its
//! own) is the main agent's choice per call (`SubAgentWorkspace`).

use crate::tools::ToolRequest;
use agent_core::hooks::ToolDispatchPolicy;

pub struct SpawnAgentParallelPolicy;

impl SpawnAgentParallelPolicy {
    fn can_run_in_parallel(request: &ToolRequest) -> bool {
        request.name == "spawn_agent"
    }
}

impl ToolDispatchPolicy for SpawnAgentParallelPolicy {
    fn parallel_indices(&self, requests: &[ToolRequest]) -> Vec<usize> {
        requests
            .iter()
            .enumerate()
            .filter(|(_, req)| Self::can_run_in_parallel(req))
            .map(|(i, _)| i)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn_agent_request(id: &str, workspace: Option<&str>) -> ToolRequest {
        let mut input = serde_json::json!({ "instructions": "test" });
        if let Some(workspace) = workspace {
            input["workspace"] = serde_json::json!(workspace);
        }
        ToolRequest {
            id: id.to_string(),
            name: "spawn_agent".to_string(),
            input,
            start_offset: None,
            end_offset: None,
        }
    }

    #[test]
    fn spawn_agents_run_in_parallel_wherever_they_work() {
        let requests = vec![
            spawn_agent_request("1", None),
            spawn_agent_request("2", Some("copy")),
            spawn_agent_request("3", Some("checkout")),
        ];
        let indices = SpawnAgentParallelPolicy.parallel_indices(&requests);
        assert_eq!(indices, vec![0, 1, 2]);
    }

    #[test]
    fn other_tools_are_sequential() {
        let requests = vec![ToolRequest {
            id: "1".to_string(),
            name: "read_files".to_string(),
            input: serde_json::json!({ "paths": ["a.txt"] }),
            start_offset: None,
            end_offset: None,
        }];
        let indices = SpawnAgentParallelPolicy.parallel_indices(&requests);
        assert!(indices.is_empty());
    }
}
