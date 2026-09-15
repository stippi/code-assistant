//! Plan persistence: snapshots the plan onto the last assistant message
//! after each successful `update_plan` call.

use crate::persistence::MessageNodeExt;
use crate::plugins::AgentAppState;
use crate::tools::ToolRequest;
use agent_core::hooks::{LoopCtx, ToolInterceptor};
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
        match ctx.conversation.last_assistant_node_mut() {
            Some(node) => {
                node.set_plan_snapshot(plan);
                trace!("Saved plan snapshot to assistant message node {}", node.id);
            }
            None => trace!("No assistant message found to save plan snapshot"),
        }
    }
}
