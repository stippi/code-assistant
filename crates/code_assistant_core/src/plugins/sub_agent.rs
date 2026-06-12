//! Dispatch policy for the `spawn_agent` tool: multiple read-only sub-agents
//! of the same turn run concurrently.

use crate::tools::ToolRequest;
use agent_core::hooks::ToolDispatchPolicy;

pub struct SpawnAgentParallelPolicy;

impl SpawnAgentParallelPolicy {
    fn can_run_in_parallel(request: &ToolRequest) -> bool {
        if request.name != "spawn_agent" {
            return false;
        }
        // Check if mode is read_only (default if not specified)
        let mode = request.input["mode"].as_str().unwrap_or("read_only");
        mode == "read_only"
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

    fn spawn_agent_request(id: &str, mode: Option<&str>) -> ToolRequest {
        let mut input = serde_json::json!({ "instructions": "test" });
        if let Some(mode) = mode {
            input["mode"] = serde_json::json!(mode);
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
    fn read_only_spawn_agents_run_in_parallel() {
        let requests = vec![
            spawn_agent_request("1", Some("read_only")),
            spawn_agent_request("2", None), // mode defaults to read_only
            spawn_agent_request("3", Some("default")),
        ];
        let indices = SpawnAgentParallelPolicy.parallel_indices(&requests);
        assert_eq!(indices, vec![0, 1]);
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
