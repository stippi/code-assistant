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
/// Runtime-owned conversation. Only the tree is writable; the linear history
/// is a derived cache, never a prompt recovery workspace or restore authority.
/// Kept crate-private so the persisted/public tree representation stays stable.
pub(crate) struct Conversation {
    nodes: std::collections::BTreeMap<NodeId, MessageNode>,
    path: ConversationPath,
    next_id: NodeId,
    history: Vec<Message>,
}

impl Default for Conversation {
    fn default() -> Self {
        Self::restore(Default::default(), Vec::new(), 1, Vec::new())
    }
}

impl Conversation {
    pub(crate) fn restore(
        nodes: std::collections::BTreeMap<NodeId, MessageNode>,
        path: ConversationPath,
        next_id: NodeId,
        legacy_messages: Vec<Message>,
    ) -> Self {
        let mut conversation = Self {
            next_id: next_id.max(nodes.keys().next_back().copied().unwrap_or(0) + 1),
            nodes,
            path,
            history: Vec::new(),
        };
        if conversation.nodes.is_empty() {
            conversation.path.clear();
            for message in legacy_messages {
                let id = conversation.reserve_id();
                conversation.append(message, id);
            }
        }
        conversation.rebuild_history();
        conversation
    }

    pub(crate) fn nodes(&self) -> &std::collections::BTreeMap<NodeId, MessageNode> {
        &self.nodes
    }

    pub(crate) fn path(&self) -> &ConversationPath {
        &self.path
    }

    pub(crate) fn next_id(&self) -> NodeId {
        self.next_id
    }

    pub(crate) fn history(&self) -> &[Message] {
        &self.history
    }

    pub(crate) fn reserve_id(&mut self) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub(crate) fn append(&mut self, message: Message, id: NodeId) {
        assert!(
            !self.nodes.contains_key(&id),
            "message node id already exists"
        );
        self.next_id = self.next_id.max(id + 1);
        self.nodes.insert(
            id,
            MessageNode {
                id,
                message,
                parent_id: self.path.last().copied(),
                created_at: SystemTime::now(),
                extension: None,
            },
        );
        self.path.push(id);
        self.rebuild_history();
    }

    /// Persistent correction of one active-path message (content, usage, etc.).
    /// Node identity, parent links, extensions and inactive branches survive.
    pub(crate) fn edit_message(&mut self, id: NodeId, edit: impl FnOnce(&mut Message)) {
        if let Some(node) = self.nodes.get_mut(&id) {
            edit(&mut node.message);
            self.rebuild_history();
        }
    }

    /// Compatibility boundary for existing hooks that take mutable tree nodes.
    /// Re-derive history once after the hook batch, including early results.
    pub(crate) fn with_nodes_mut<T>(
        &mut self,
        edit: impl FnOnce(&mut std::collections::BTreeMap<NodeId, MessageNode>, &ConversationPath) -> T,
    ) -> T {
        let result = edit(&mut self.nodes, &self.path);
        self.rebuild_history();
        result
    }

    fn rebuild_history(&mut self) {
        self.history = self
            .path
            .iter()
            .filter_map(|id| self.nodes.get(id))
            .map(|node| node.message.clone())
            .collect();
    }
}
