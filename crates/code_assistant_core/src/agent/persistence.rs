use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Mutex;

#[cfg(test)]
use crate::types::PlanState;
#[cfg(test)]
use agent_core::types::ToolExecution;
#[cfg(test)]
use llm::Message;

use crate::session::{SessionManager, SessionState};

// The snapshot shape and trait the loop persists through live in the agent
// core.
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
            active_skills: state.active_skills.clone(),
            config: state.session_config.clone(),
            next_request_id: Some(snapshot.next_request_id),
            model_config: state.model_config.clone(),
        })
    }
}

/// Mock implementation for testing
#[cfg(test)]
#[derive(Default)]
pub struct MockStatePersistence {
    pub save_count: usize,
    pub last_saved_messages: Option<Vec<Message>>,
    pub last_saved_tool_executions: Option<Vec<ToolExecution>>,
    pub last_saved_plan: Option<PlanState>,
}

#[cfg(test)]
impl MockStatePersistence {
    pub fn new() -> Self {
        Self::default()
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
    pub fn new(
        inner: Box<dyn AgentStatePersistence>,
        ui: Arc<dyn crate::ui::UserInterface>,
    ) -> Self {
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
