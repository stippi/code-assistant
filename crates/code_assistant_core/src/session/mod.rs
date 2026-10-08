use crate::persistence::{ConversationPath, MessageNode, NodeId, SessionModelConfig};
use crate::types::{PlanState, ToolSyntax};
use crate::utils::serde_fallback::{or_default, or_else};
use agent_core::types::SerializedToolExecution;
use agent_core::types::ToolExecution;
use llm::Message;
use sandbox::SandboxPolicy;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tools_core::permissions::PermissionTier;

// New session management architecture
pub mod browsers;
pub mod event_stream;
pub mod idle_handoff;
pub mod instance;
pub mod lifecycle;
pub mod manager;
pub mod new_context;
pub mod permissions;
pub mod pull_request;
pub mod questions;
pub mod service;
pub mod sleep_inhibitor;
pub mod turn;
pub mod wakeup;
pub mod watcher;

// Main session manager, the UI→core command facade on top of it, and the
// core→UI broadcast stream
pub use event_stream::{EventPayload, EventStream, SessionEvent, StreamError, Subscription};
pub use manager::SessionManager;
pub use service::SessionService;
pub use service::{RepoReview, ReviewMode, ReviewScanState, WorktreeListing};
pub use turn::{
    ResourceRef, ToolRecord, TurnDispatch, TurnHandle, TurnOutcome, TurnRequest, TurnStatus,
    TurnUsage,
};
pub use wakeup::{SessionWakeups, WakeupHandle, spawn_wakeup_scheduler};

/// Owned snapshot of everything a frontend needs to render a session.
///
/// Returned by `SessionService::load_session`. Frontends render it and then
/// apply subsequent events for the session from the broadcast stream; a
/// lagged subscriber recovers by fetching a fresh snapshot.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    pub session_id: String,
    /// Transcript of the active path. When an agent response is currently
    /// streaming, the last entry is the in-flight partial assistant message.
    pub messages: Vec<crate::ui::ui_events::MessageData>,
    pub tool_results: Vec<crate::ui::ui_events::ToolResultData>,
    pub plan: PlanState,
    pub activity_state: instance::SessionActivityState,
    pub metadata: crate::persistence::ChatMetadata,
    /// Text summary of a queued (pending) user message, if any.
    pub pending_message: Option<String>,
    pub current_model: String,
    pub allowed_models: Vec<String>,
    pub sandbox_policy: SandboxPolicy,
    pub permission_tier: PermissionTier,
    /// MCP servers available to this session and whether each is enabled for
    /// it (global `mcp-servers.json` plus the project's trusted `.mcp.json`).
    pub mcp_servers: Vec<crate::ui::ui_events::McpServerToggle>,
    /// Permission requests still awaiting an answer; a connecting frontend
    /// should render prompts for them.
    pub pending_permission_requests: Vec<permissions::ToolPermissionRequestData>,
    /// The open `/new` / `/handoff` target question, if any.
    pub pending_new_context_target: Option<new_context::NewContextTargetRequest>,
    /// `ask_question` requests still awaiting answers.
    pub pending_questions: Vec<questions::UserQuestionRequest>,
}

impl SessionSnapshot {
    /// Render this snapshot as the canonical connect-event sequence.
    ///
    /// Frontends that ingest [`crate::ui::UiEvent`]s through a single queue
    /// can apply a snapshot by replaying these events in order.
    pub fn connect_events(&self) -> Vec<crate::ui::UiEvent> {
        use crate::ui::UiEvent;

        let mut events = vec![
            UiEvent::SetMessages {
                messages: self.messages.clone(),
                session_id: Some(self.session_id.clone()),
                tool_results: self.tool_results.clone(),
            },
            UiEvent::UpdatePlan {
                plan: self.plan.clone(),
            },
            UiEvent::UpdateSessionActivityState {
                session_id: self.session_id.clone(),
                activity_state: self.activity_state.clone(),
            },
        ];
        // Show the error banner when connecting to an errored session.
        if let instance::SessionActivityState::Errored { message } = &self.activity_state {
            events.push(UiEvent::DisplayError {
                message: message.clone(),
            });
        }
        events.push(UiEvent::UpdateSessionMetadata {
            metadata: self.metadata.clone(),
        });
        events.push(UiEvent::UpdatePendingMessage {
            message: self.pending_message.clone(),
        });
        events.push(UiEvent::UpdateCurrentModel {
            model_name: self.current_model.clone(),
        });
        events.push(UiEvent::UpdateSandboxPolicy {
            policy: self.sandbox_policy.clone(),
        });
        events.push(UiEvent::UpdatePermissionTier {
            tier: self.permission_tier,
        });
        events.push(UiEvent::UpdateMcpServers {
            servers: self.mcp_servers.clone(),
        });
        events.push(UiEvent::UpdateAllowedModels {
            models: self.allowed_models.clone(),
        });
        for request in &self.pending_permission_requests {
            events.push(UiEvent::RequestToolPermission {
                request: request.clone(),
            });
        }
        for request in &self.pending_questions {
            events.push(UiEvent::RequestUserQuestions {
                request: request.clone(),
            });
        }
        if let Some(request) = &self.pending_new_context_target {
            events.push(UiEvent::RequestNewContextTarget {
                request: request.clone(),
            });
        }
        events
    }
}

/// Static configuration stored with each session.
///
/// Enum-valued settings fall back to their default when the stored value is
/// unknown to this build (written by a newer build or naming a removed
/// option), so a stale setting never prevents loading the session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_path: Option<PathBuf>,
    #[serde(default)]
    pub initial_project: String,
    #[serde(default, deserialize_with = "or_default")]
    pub tool_syntax: ToolSyntax,
    #[serde(default)]
    pub use_diff_blocks: bool,
    #[serde(default, deserialize_with = "or_default")]
    pub sandbox_policy: SandboxPolicy,
    /// When to ask the user for permission before running a tool.
    #[serde(default, deserialize_with = "permission_tier_or_strictest")]
    pub permission_tier: PermissionTier,
    /// If set, the session operates inside this git worktree instead of `init_path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<PathBuf>,
    /// The git branch name associated with this session (e.g. `feature/login`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// MCP servers deactivated for this session (by config name). Their tools
    /// are not offered to or callable by this session's agent, without
    /// affecting other sessions. This filters the *tool set* only — the server
    /// is still launched when the shared registry is built (registries span
    /// sessions), so disabling here hides a server's tools rather than
    /// preventing its process from starting. Empty by default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_mcp_servers: Vec<String>,
}

/// An unknown tier (e.g. one only another build offers) must not silently
/// drop the session to the permissive default; ask before every tool instead.
fn permission_tier_or_strictest<'de, D>(deserializer: D) -> Result<PermissionTier, D::Error>
where
    D: serde::Deserializer<'de>,
{
    or_else(deserializer, || PermissionTier::AllTools)
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            init_path: None,
            initial_project: String::new(),
            tool_syntax: ToolSyntax::default(),
            use_diff_blocks: false,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            permission_tier: PermissionTier::default(),
            worktree_path: None,
            branch: None,
            disabled_mcp_servers: Vec::new(),
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
    /// Names of skills activated on the active path (progressive disclosure).
    pub active_skills: Vec<String>,
    pub config: SessionConfig,
    pub next_request_id: Option<u64>,
    pub model_config: Option<SessionModelConfig>,
}

/// What a running agent commits after each change: the conversation delta
/// since its previous checkpoint plus the run-owned fields. Session settings
/// are never part of it; the session manager owns those.
pub struct SessionCheckpoint<'a> {
    pub session_id: &'a str,
    pub name: &'a str,
    /// Nodes appended or edited since the previous checkpoint.
    pub changed_nodes: &'a [&'a MessageNode],
    pub active_path: &'a [NodeId],
    pub next_node_id: NodeId,
    /// Journal entries recorded or updated since the previous checkpoint.
    pub changed_executions: Vec<SerializedToolExecution>,
    pub plan: &'a PlanState,
    pub active_skills: &'a [String],
    pub next_request_id: u64,
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
            active_skills: Vec::new(),
            config,
            next_request_id: Some(max_request_id + 1),
            model_config: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_config_survives_unknown_setting_values() {
        let config: SessionConfig = serde_json::from_str(
            r#"{
                "initial_project": "demo",
                "tool_syntax": "Yaml",
                "sandbox_policy": {"mode": "future-policy"},
                "permission_tier": "auto",
                "branch": "feature/x"
            }"#,
        )
        .unwrap();

        assert_eq!(config.initial_project, "demo");
        assert_eq!(config.tool_syntax, ToolSyntax::Native);
        assert_eq!(config.sandbox_policy, SandboxPolicy::DangerFullAccess);
        assert_eq!(config.permission_tier, PermissionTier::AllTools);
        assert_eq!(config.branch.as_deref(), Some("feature/x"));
    }

    #[test]
    fn session_config_keeps_known_setting_values() {
        let original = SessionConfig {
            tool_syntax: ToolSyntax::Caret,
            sandbox_policy: SandboxPolicy::ReadOnly,
            permission_tier: PermissionTier::OutwardTools,
            ..SessionConfig::default()
        };
        let json = serde_json::to_string(&original).unwrap();
        let config: SessionConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(config.tool_syntax, ToolSyntax::Caret);
        assert_eq!(config.sandbox_policy, SandboxPolicy::ReadOnly);
        assert_eq!(config.permission_tier, PermissionTier::OutwardTools);
    }
}
