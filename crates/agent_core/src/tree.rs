//! The conversation tree the agent loop maintains: messages as nodes with
//! parent links, plus the active path through them (branching).

use llm::Message;
use serde::{Deserialize, Serialize};
use std::time::SystemTime;

/// Unique identifier for a message node within a session
pub type NodeId = u64;

/// A path through the conversation tree (list of node IDs from root to leaf)
pub type ConversationPath = Vec<NodeId>;

/// A single message node in the conversation tree
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MessageNode {
    /// Unique ID within this session
    pub id: NodeId,

    /// The actual message content
    pub message: Message,

    /// Parent node ID (None for root/first message)
    pub parent_id: Option<NodeId>,

    /// Creation timestamp (for ordering siblings)
    pub created_at: SystemTime,

    /// Application-specific data riding on this node (e.g. code-assistant
    /// stores a plan snapshot when the plan changed in this message's
    /// response). The `plan_snapshot` alias keeps old session files loading.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "plan_snapshot"
    )]
    pub extension: Option<serde_json::Value>,
}
