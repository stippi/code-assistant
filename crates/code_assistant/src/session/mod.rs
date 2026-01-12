use crate::agent::ToolExecution;
use crate::persistence::{ConversationPath, MessageNode, NodeId, SessionModelConfig};
use crate::types::{PlanState, ToolSyntax};
use llm::Message;
use sandbox::SandboxPolicy;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

// New session management architecture
pub mod instance;
pub mod manager;

// Main session manager
pub use manager::SessionManager;

/// Static configuration stored with each session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_path: Option<PathBuf>,
    #[serde(default)]
    pub initial_project: String,
    #[serde(default = "default_tool_syntax")]
    pub tool_syntax: ToolSyntax,
    #[serde(default)]
    pub use_diff_blocks: bool,
    #[serde(default)]
    pub sandbox_policy: SandboxPolicy,
}

fn default_tool_syntax() -> ToolSyntax {
    ToolSyntax::Native
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            init_path: None,
            initial_project: String::new(),
            tool_syntax: default_tool_syntax(),
            use_diff_blocks: false,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
        }
    }
}

/// State data needed to restore an agent session.
///
/// This struct supports both the new tree-based branching structure and
/// a legacy linear message list for backward compatibility.
#[derive(Debug, Clone)]
pub struct SessionState {
    pub session_id: String,
    pub name: String,

    // ========================================================================
    // Branching: Tree-based message storage
    // ========================================================================
    /// All message nodes in the session (tree structure)
    pub message_nodes: BTreeMap<NodeId, MessageNode>,

    /// The currently active path through the tree
    pub active_path: ConversationPath,

    /// Counter for generating unique node IDs
    pub next_node_id: NodeId,

    // ========================================================================
    // Legacy: For backward compatibility during transition
    // ========================================================================
    /// Linearized message history (derived from active_path for convenience)
    /// This is kept in sync with the tree and used by the agent loop
    pub messages: Vec<Message>,

    pub tool_executions: Vec<ToolExecution>,
    pub plan: PlanState,
    pub config: SessionConfig,
    pub next_request_id: Option<u64>,
    pub model_config: Option<SessionModelConfig>,
}

impl SessionState {
    /// Get the linearized message history for the active path.
    /// This returns cloned messages from the tree structure.
    #[allow(dead_code)] // Will be used more extensively in Phase 4
    pub fn get_active_messages(&self) -> Vec<Message> {
        self.active_path
            .iter()
            .filter_map(|id| self.message_nodes.get(id))
            .map(|node| node.message.clone())
            .collect()
    }

    /// Ensure the messages vec is in sync with the tree.
    /// Call this after modifying the tree structure.
    #[allow(dead_code)] // Will be used more extensively in Phase 4
    pub fn sync_messages_from_tree(&mut self) {
        self.messages = self.get_active_messages();
    }

    /// Create a SessionState from a linear list of messages.
    /// This is primarily for tests and backward compatibility.
    /// The messages are converted to a tree structure with a single linear path.
    #[allow(dead_code)] // Used by tests
    pub fn from_messages(
        session_id: impl Into<String>,
        name: impl Into<String>,
        messages: Vec<Message>,
        config: SessionConfig,
    ) -> Self {
        let mut message_nodes = BTreeMap::new();
        let mut active_path = Vec::new();
        let mut next_node_id: NodeId = 1;
        let mut parent_id: Option<NodeId> = None;

        // Infer next_request_id from messages
        let max_request_id = messages
            .iter()
            .filter_map(|m| m.request_id)
            .max()
            .unwrap_or(0);

        for message in &messages {
            let node_id = next_node_id;
            next_node_id += 1;

            let node = crate::persistence::MessageNode {
                id: node_id,
                message: message.clone(),
                parent_id,
                created_at: std::time::SystemTime::now(),
                plan_snapshot: None,
            };

            message_nodes.insert(node_id, node);
            active_path.push(node_id);
            parent_id = Some(node_id);
        }

        Self {
            session_id: session_id.into(),
            name: name.into(),
            message_nodes,
            active_path,
            next_node_id,
            messages,
            tool_executions: Vec::new(),
            plan: PlanState::default(),
            config,
            next_request_id: Some(max_request_id + 1),
            model_config: None,
        }
    }
}
