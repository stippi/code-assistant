//! Skill activation tracking: records the skill a successful `read_skill`
//! loaded into `AgentAppState.active_skills` and snapshots the updated set
//! onto the last assistant message node so it reconstructs correctly when
//! switching branches (mirrors [`crate::plugins::PlanSnapshotHook`]).

use crate::persistence::MessageNodeExt;
use crate::plugins::AgentAppState;
use crate::tools::ToolRequest;
use agent_core::hooks::{LoopCtx, ToolInterceptor};
use tracing::trace;

pub struct SkillSnapshotHook;

impl ToolInterceptor for SkillSnapshotHook {
    fn after_tool_success(&self, request: &ToolRequest, ctx: &mut LoopCtx) {
        if request.name != "read_skill" {
            return;
        }

        let Some(name) = request.input["name"].as_str() else {
            return;
        };
        let name = name.trim();
        if name.is_empty() {
            return;
        }

        // Record the activation (deduplicated, activation order preserved).
        let state = AgentAppState::of(ctx.extensions);
        if !state.active_skills.iter().any(|s| s == name) {
            state.active_skills.push(name.to_string());
        }
        let active_skills = state.active_skills.clone();

        // Snapshot onto the last assistant node for branch reconstruction.
        for &node_id in ctx.active_path.iter().rev() {
            if let Some(node) = ctx.message_nodes.get(&node_id) {
                if node.message.role == llm::MessageRole::Assistant {
                    if let Some(node_mut) = ctx.message_nodes.get_mut(&node_id) {
                        node_mut.set_active_skills_snapshot(active_skills);
                        trace!("Saved active-skills snapshot to assistant node {}", node_id);
                    }
                    return;
                }
            }
        }
        trace!("No assistant message found to save active-skills snapshot");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::MessageNode;
    use crate::session::SessionConfig;
    use agent_core::hooks::LoopCtx;
    use llm::Message;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::time::SystemTime;

    fn read_skill_request(name: &str) -> ToolRequest {
        ToolRequest {
            id: "t1".to_string(),
            name: "read_skill".to_string(),
            input: json!({ "project": "p", "name": name }),
            start_offset: None,
            end_offset: None,
        }
    }

    #[test]
    fn records_activation_and_snapshots_onto_assistant_node() {
        let registry = crate::tools::test_registry();
        let mut message_nodes = BTreeMap::new();
        message_nodes.insert(
            1,
            MessageNode {
                id: 1,
                message: Message::new_assistant("loading"),
                parent_id: None,
                created_at: SystemTime::now(),
                extension: None,
            },
        );
        let active_path = vec![1];
        let mut tool_executions = Vec::new();
        let mut state = AgentAppState::new(SessionConfig::default());

        {
            let mut ctx = LoopCtx {
                tool_executions: &mut tool_executions,
                message_nodes: &mut message_nodes,
                active_path: &active_path,
                registry: registry.as_ref(),
                extensions: &mut state,
            };
            SkillSnapshotHook.after_tool_success(&read_skill_request("alpha"), &mut ctx);
        }

        assert_eq!(state.active_skills, vec!["alpha".to_string()]);
        assert_eq!(
            message_nodes.get(&1).unwrap().active_skills_snapshot(),
            Some(vec!["alpha".to_string()])
        );

        // Re-activating the same skill does not duplicate it.
        {
            let mut ctx = LoopCtx {
                tool_executions: &mut tool_executions,
                message_nodes: &mut message_nodes,
                active_path: &active_path,
                registry: registry.as_ref(),
                extensions: &mut state,
            };
            SkillSnapshotHook.after_tool_success(&read_skill_request("alpha"), &mut ctx);
        }
        assert_eq!(state.active_skills, vec!["alpha".to_string()]);
    }

    #[test]
    fn ignores_other_tools() {
        let registry = crate::tools::test_registry();
        let mut message_nodes = BTreeMap::new();
        let active_path: Vec<u64> = Vec::new();
        let mut tool_executions = Vec::new();
        let mut state = AgentAppState::new(SessionConfig::default());

        let request = ToolRequest {
            id: "t1".to_string(),
            name: "read_files".to_string(),
            input: json!({ "project": "p", "paths": ["a"] }),
            start_offset: None,
            end_offset: None,
        };
        {
            let mut ctx = LoopCtx {
                tool_executions: &mut tool_executions,
                message_nodes: &mut message_nodes,
                active_path: &active_path,
                registry: registry.as_ref(),
                extensions: &mut state,
            };
            SkillSnapshotHook.after_tool_success(&request, &mut ctx);
        }

        assert!(state.active_skills.is_empty());
    }
}
