use agent_core::types::ToolExecution;
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
pub mod sleep_inhibitor;
pub mod watcher;

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
    /// If set, the session operates inside this git worktree instead of `init_path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<PathBuf>,
    /// The git branch name associated with this session (e.g. `feature/login`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
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
            worktree_path: None,
            branch: None,
        }
    }
}

impl SessionConfig {
    /// Returns the effective project path: worktree path if set, otherwise init_path.
    ///
    /// This is the directory where the agent should operate — file tools,
    /// command execution, and the system prompt file tree all use this path.
    pub fn effective_project_path(&self) -> Option<&PathBuf> {
        self.worktree_path.as_ref().or(self.init_path.as_ref())
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
    /// Build the chat metadata that describes this state, as shown in the
    /// session list. `created_at`/`updated_at` and the token limit are
    /// placeholders the persistence layer overrides.
    pub fn build_metadata(&self) -> crate::persistence::ChatMetadata {
        use std::time::SystemTime;

        // Calculate total usage and find last usage across all messages
        let mut total_usage = llm::Usage::zero();
        let mut last_usage = llm::Usage::zero();

        for message in &self.messages {
            if let Some(usage) = &message.usage {
                total_usage.input_tokens += usage.input_tokens;
                total_usage.output_tokens += usage.output_tokens;
                total_usage.cache_creation_input_tokens += usage.cache_creation_input_tokens;
                total_usage.cache_read_input_tokens += usage.cache_read_input_tokens;

                // For assistant messages, update last usage (most recent wins)
                if matches!(message.role, llm::MessageRole::Assistant) {
                    last_usage = usage.clone();
                }
            }
        }

        // Compute resumability from the current in-memory history.
        // While the agent is running this is largely cosmetic — the UI
        // only acts on it once the session is idle — but we still want
        // it to reflect the truth as soon as a save runs after the
        // agent finishes.
        let messages_ref: Vec<&llm::Message> = self.messages.iter().collect();
        let is_resumable = crate::persistence::is_resumable_from_messages(messages_ref.as_slice());

        crate::persistence::ChatMetadata {
            id: self.session_id.clone(),
            name: self.name.clone(), // Empty string if not named yet
            created_at: SystemTime::now(), // Will be overridden by persistence
            updated_at: SystemTime::now(),
            message_count: self.messages.len(),
            total_usage,
            last_usage,
            tokens_limit: None, // Will be updated by the persistence layer
            tool_syntax: self.config.tool_syntax,
            initial_project: if self.config.initial_project.is_empty() {
                "unknown".to_string()
            } else {
                self.config.initial_project.clone()
            },
            plan_collapsed: false, // Agent doesn't track UI state
            is_resumable,
        }
    }
}

#[cfg(test)]
impl SessionState {
    /// Create a SessionState from a linear list of messages.
    /// This is primarily for tests and backward compatibility.
    /// The messages are converted to a tree structure with a single linear path.
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
                extension: None,
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
