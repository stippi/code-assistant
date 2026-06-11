//! Plan persistence: snapshots the plan onto the last assistant message
//! after each successful `update_plan` call.

use crate::agent::hooks::{LoopCtx, ToolInterceptor};
use crate::plugins::AgentAppState;
use crate::tools::ToolRequest;
use tracing::trace;

/// Stores a plan snapshot in the message tree so the plan can be
/// reconstructed correctly when switching branches.
pub struct PlanSnapshotHook;

impl ToolInterceptor for PlanSnapshotHook {
    fn after_tool_success(&self, request: &ToolRequest, ctx: &mut LoopCtx) {
        if request.name != "update_plan" {
            return;
        }

        let plan = AgentAppState::of_ref(&*ctx.extensions).plan.clone();

        // Find the last assistant message in the active path
        for &node_id in ctx.active_path.iter().rev() {
            if let Some(node) = ctx.message_nodes.get(&node_id) {
                if node.message.role == llm::MessageRole::Assistant {
                    // Found it - set the snapshot
                    if let Some(node_mut) = ctx.message_nodes.get_mut(&node_id) {
                        node_mut.plan_snapshot = Some(plan);
                        trace!("Saved plan snapshot to assistant message node {}", node_id);
                    }
                    return;
                }
            }
        }
        // No assistant message found - this shouldn't happen in normal flow
        trace!("No assistant message found to save plan snapshot");
    }
}
