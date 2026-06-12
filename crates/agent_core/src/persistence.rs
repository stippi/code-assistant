//! Core-shaped persistence: the loop saves what it owns — the conversation
//! tree, the linearized history, the tool executions, and the id counters.
//! Application-level fields travel separately through the extension state
//! and are assembled into the application's storage format by its adapter.

use crate::tree::{ConversationPath, MessageNode, NodeId};
use crate::types::ToolExecution;
use anyhow::Result;
use std::any::Any;
use std::collections::BTreeMap;

/// What the agent loop itself knows about and persists.
pub struct AgentSnapshot {
    pub session_id: Option<String>,
    pub message_nodes: BTreeMap<NodeId, MessageNode>,
    pub active_path: ConversationPath,
    pub next_node_id: NodeId,
    /// Linearized message history (derived from `active_path`).
    pub messages: Vec<llm::Message>,
    pub tool_executions: Vec<ToolExecution>,
    pub next_request_id: u64,
}

/// Persistence used by the agent loop: it saves the loop's snapshot, with
/// the application fields supplied by the extension state.
pub trait SnapshotPersistence: Send + Sync {
    fn save(&mut self, snapshot: AgentSnapshot, extensions: &(dyn Any + Send)) -> Result<()>;
}
