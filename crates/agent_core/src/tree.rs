//! The conversation tree the agent loop maintains: messages as nodes with
//! parent links, plus the active path through them (branching).

use llm::Message;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
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

/// The conversation a running agent owns: the tree, the active path, the id
/// counter, and which nodes changed since the last checkpoint.
///
/// Only the tree is written to. The linear history is derived from the
/// active path whenever it is needed, so it can never disagree with the tree.
pub struct Conversation {
    nodes: BTreeMap<NodeId, MessageNode>,
    path: ConversationPath,
    next_id: NodeId,
    changed: BTreeSet<NodeId>,
}

impl Default for Conversation {
    fn default() -> Self {
        Self::restore(BTreeMap::new(), Vec::new(), 1, Vec::new())
    }
}

impl Conversation {
    /// Rebuild the conversation from persisted state. A session without a
    /// tree imports its legacy linear messages as a single branch; those
    /// imported nodes count as changed so the next checkpoint persists them.
    pub fn restore(
        nodes: BTreeMap<NodeId, MessageNode>,
        path: ConversationPath,
        next_id: NodeId,
        legacy_messages: Vec<Message>,
    ) -> Self {
        let mut conversation = Self {
            next_id: next_id.max(nodes.keys().next_back().copied().unwrap_or(0) + 1),
            nodes,
            path,
            changed: BTreeSet::new(),
        };
        if conversation.nodes.is_empty() {
            conversation.path.clear();
            for message in legacy_messages {
                let id = conversation.reserve_id();
                conversation.append(message, id);
            }
        }
        conversation
    }

    pub fn nodes(&self) -> &BTreeMap<NodeId, MessageNode> {
        &self.nodes
    }

    pub fn path(&self) -> &[NodeId] {
        &self.path
    }

    pub fn next_id(&self) -> NodeId {
        self.next_id
    }

    pub fn node(&self, id: NodeId) -> Option<&MessageNode> {
        self.nodes.get(&id)
    }

    /// Mutable access to a node. The node counts as changed for the next
    /// checkpoint; callers are responsible for keeping parent links intact.
    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut MessageNode> {
        let node = self.nodes.get_mut(&id)?;
        self.changed.insert(id);
        Some(node)
    }

    /// The most recent assistant message on the active path.
    pub fn last_assistant_id(&self) -> Option<NodeId> {
        self.path.iter().rev().copied().find(|id| {
            self.nodes
                .get(id)
                .is_some_and(|node| node.message.role == llm::MessageRole::Assistant)
        })
    }

    pub fn last_assistant_node_mut(&mut self) -> Option<&mut MessageNode> {
        let id = self.last_assistant_id()?;
        self.node_mut(id)
    }

    /// The messages on the active path, in order.
    pub fn active_messages(&self) -> impl Iterator<Item = &Message> + '_ {
        self.path
            .iter()
            .filter_map(|id| self.nodes.get(id))
            .map(|node| &node.message)
    }

    /// The linear history as an owned list.
    pub fn history(&self) -> Vec<Message> {
        self.active_messages().cloned().collect()
    }

    pub fn reserve_id(&mut self) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Append a message to the active path as a child of its last node.
    pub fn append(&mut self, message: Message, id: NodeId) {
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
        self.changed.insert(id);
    }

    /// Nodes appended or edited since the last checkpoint, in id order.
    pub fn changed_nodes(&self) -> impl Iterator<Item = &MessageNode> + '_ {
        self.changed.iter().filter_map(|id| self.nodes.get(id))
    }

    /// Forget the change marks after a successful checkpoint.
    pub fn mark_checkpointed(&mut self) {
        self.changed.clear();
    }
}
