//! Core-shaped persistence: after every change the loop hands its
//! persistence the delta since the previous checkpoint — the nodes and
//! journal entries that changed, plus the small always-current fields.
//! Prompt-only repairs and context-recovery projections are never part of
//! it. Application-level fields travel separately through the extension
//! state and are assembled into the application's storage format by its
//! adapter.

use crate::tree::{MessageNode, NodeId};
use crate::types::ToolExecution;
use anyhow::Result;
use std::any::Any;

/// What changed since the previous checkpoint of this run.
pub struct AgentCheckpoint<'a> {
    pub session_id: &'a str,
    /// Nodes appended or edited since the previous checkpoint.
    pub changed_nodes: Vec<&'a MessageNode>,
    pub active_path: &'a [NodeId],
    pub next_node_id: NodeId,
    /// Journal entries recorded or updated since the previous checkpoint.
    pub changed_executions: Vec<&'a ToolExecution>,
    pub next_request_id: u64,
}

/// Persistence used by the agent loop.
pub trait CheckpointPersistence: Send + Sync {
    /// Merge the checkpoint into the stored session. A call is atomic: on
    /// `Err` nothing of it is stored, and the loop keeps the changes marked
    /// for its next attempt.
    fn commit(
        &mut self,
        checkpoint: &AgentCheckpoint<'_>,
        extensions: &(dyn Any + Send),
    ) -> Result<()>;
}
