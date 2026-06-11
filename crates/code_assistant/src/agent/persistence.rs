use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::Mutex;
use tracing::{debug, info};

#[cfg(test)]
use crate::agent::ToolExecution;
#[cfg(test)]
use crate::types::PlanState;
#[cfg(test)]
use llm::Message;

use crate::persistence::{ChatSession, SerializedToolExecution};
use crate::session::{SessionManager, SessionState};

// The snapshot shape and trait the loop persists through live in the agent
// core (Phase 4 step 2).
pub use agent_core::{AgentSnapshot, SnapshotPersistence};

/// Trait for persisting agent state
/// This abstracts away the storage mechanism from the Agent implementation
pub trait AgentStatePersistence: Send + Sync {
    /// Save the current agent state
    fn save_agent_state(&mut self, state: SessionState) -> Result<()>;
}

/// Assembles code-assistant's [`SessionState`] from the loop snapshot plus
/// [`crate::plugins::AgentAppState`], and forwards it to an
/// [`AgentStatePersistence`] backend. Snapshots without a session id are not
/// persisted (matching the loop's previous behavior for anonymous agents).
pub struct SessionStateAdapter {
    inner: Box<dyn AgentStatePersistence>,
}

impl SessionStateAdapter {
    pub fn new(inner: Box<dyn AgentStatePersistence>) -> Self {
        Self { inner }
    }
}

impl SnapshotPersistence for SessionStateAdapter {
    fn save(
        &mut self,
        snapshot: AgentSnapshot,
        extensions: &(dyn std::any::Any + Send),
    ) -> Result<()> {
        let Some(session_id) = snapshot.session_id else {
            return Ok(());
        };
        let state = crate::plugins::AgentAppState::of_ref(extensions);

        self.inner.save_agent_state(SessionState {
            session_id,
            name: state.session_name.clone(),
            message_nodes: snapshot.message_nodes,
            active_path: snapshot.active_path,
            next_node_id: snapshot.next_node_id,
            messages: snapshot.messages,
            tool_executions: snapshot.tool_executions,
            plan: state.plan.clone(),
            config: state.session_config.clone(),
            next_request_id: Some(snapshot.next_request_id),
            model_config: state.model_config.clone(),
        })
    }
}

/// Mock implementation for testing
#[cfg(test)]
pub struct MockStatePersistence {
    pub save_count: usize,
    pub last_saved_messages: Option<Vec<Message>>,
    pub last_saved_tool_executions: Option<Vec<ToolExecution>>,
    pub last_saved_plan: Option<PlanState>,
}

#[cfg(test)]
impl MockStatePersistence {
    pub fn new() -> Self {
        Self {
            save_count: 0,
            last_saved_messages: None,
            last_saved_tool_executions: None,
            last_saved_plan: None,
        }
    }
}

#[cfg(test)]
impl AgentStatePersistence for MockStatePersistence {
    fn save_agent_state(&mut self, state: SessionState) -> Result<()> {
        self.save_count += 1;
        self.last_saved_messages = Some(state.messages);
        self.last_saved_tool_executions = Some(state.tool_executions);
        self.last_saved_plan = Some(state.plan);
        Ok(())
    }
}

/// Decorates a persistence backend with the session-metadata UI update that
/// accompanies every save while an agent runs. Keeps the agent loop free of
/// the `ChatMetadata` concern: the metadata is derived from the saved state.
pub struct MetadataNotifyingPersistence {
    inner: Box<dyn AgentStatePersistence>,
    ui: Arc<dyn crate::ui::UserInterface>,
}

impl MetadataNotifyingPersistence {
    pub fn new(inner: Box<dyn AgentStatePersistence>, ui: Arc<dyn crate::ui::UserInterface>) -> Self {
        Self { inner, ui }
    }
}

impl AgentStatePersistence for MetadataNotifyingPersistence {
    fn save_agent_state(&mut self, state: SessionState) -> Result<()> {
        let metadata = state.build_metadata();
        self.inner.save_agent_state(state)?;

        // Send updated session metadata to UI (fire-and-forget)
        let _ = tokio::runtime::Handle::try_current().map(|handle| {
            let ui = self.ui.clone();
            handle.spawn(async move {
                let _ = ui
                    .send_event(crate::ui::UiEvent::UpdateSessionMetadata { metadata })
                    .await;
            });
        });

        Ok(())
    }
}

/// Session-specific wrapper that implements AgentStatePersistence
/// This allows agents to save state to a specific session without the SessionManager
/// needing to track a single "current" session (which would break concurrent agents)
pub struct SessionStatePersistence {
    session_manager: Arc<Mutex<SessionManager>>,
}

impl SessionStatePersistence {
    pub fn new(session_manager: Arc<Mutex<SessionManager>>) -> Self {
        Self { session_manager }
    }
}

impl AgentStatePersistence for SessionStatePersistence {
    fn save_agent_state(&mut self, state: SessionState) -> Result<()> {
        // Use blocking_lock to avoid async context issues
        // This is safe because we're in a background task context
        let mut session_manager = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.session_manager.lock())
        });
        session_manager.save_session_state(state)
    }
}

#[allow(dead_code)]
const STATE_FILE: &str = ".code-assistant.state.json";

/// Simple file-based persistence that saves agent state to a single JSON file
/// This is used for terminal mode with the --continue-task flag
#[derive(Clone)]
pub struct FileStatePersistence {
    state_file_path: PathBuf,
}

impl FileStatePersistence {
    #[allow(dead_code)]
    pub fn new(working_dir: &Path) -> Self {
        let state_file_path = working_dir.join(STATE_FILE);
        info!("Using state file: {}", state_file_path.display());
        Self { state_file_path }
    }

    /// Load agent state from the state file if it exists
    #[allow(dead_code)]
    pub fn load_agent_state(&self) -> Result<Option<ChatSession>> {
        if !self.state_file_path.exists() {
            debug!(
                "State file does not exist: {}",
                self.state_file_path.display()
            );
            return Ok(None);
        }

        debug!(
            "Loading agent state from {}",
            self.state_file_path.display()
        );
        let json = std::fs::read_to_string(&self.state_file_path)?;
        let mut session: ChatSession = serde_json::from_str(&json)?;
        session.ensure_config()?;

        info!(
            "Loaded agent state with {} messages",
            session.messages.len()
        );
        Ok(Some(session))
    }

    /// Check if the state file exists
    #[allow(dead_code)]
    pub fn has_saved_state(&self) -> bool {
        self.state_file_path.exists()
    }
}

impl AgentStatePersistence for FileStatePersistence {
    fn save_agent_state(&mut self, state: SessionState) -> Result<()> {
        debug!("Saving agent state to {}", self.state_file_path.display());

        // Convert tool executions to serialized form
        let SessionState {
            session_id,
            name,
            message_nodes,
            active_path,
            next_node_id,
            messages: _,
            tool_executions,
            plan,
            config,
            next_request_id,
            model_config,
        } = state;

        let serialized_executions: Result<Vec<SerializedToolExecution>> =
            tool_executions.iter().map(|te| te.serialize()).collect();

        let serialized_executions = serialized_executions?;

        // Create a ChatSession with the current state
        let mut session = ChatSession::new_empty(session_id, name, config, model_config);

        // Store tree structure
        session.message_nodes = message_nodes;
        session.active_path = active_path;
        session.next_node_id = next_node_id;

        // Clear legacy messages (tree is authoritative)
        session.messages.clear();

        session.tool_executions = serialized_executions;
        session.plan = plan;
        session.next_request_id = next_request_id.unwrap_or(0);
        session.updated_at = SystemTime::now();

        // Save atomically
        crate::utils::file_utils::atomic_write_json(&self.state_file_path, &session)?;

        debug!("Agent state saved successfully");
        Ok(())
    }
}
