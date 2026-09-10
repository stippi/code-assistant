//! How a running agent's checkpoints reach the session store.

use crate::session::{SessionCheckpoint, SessionManager};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Mutex;

// The checkpoint shape and trait the loop persists through live in the agent
// core.
pub use agent_core::{AgentCheckpoint, CheckpointPersistence};

/// Application-side checkpoint sink: receives the loop's delta together with
/// the application fields that ride along.
pub trait AgentStatePersistence: Send + Sync {
    /// Merge one checkpoint into the stored session. Atomic per call.
    fn commit_checkpoint(&mut self, checkpoint: SessionCheckpoint<'_>) -> Result<()>;
}

/// Assembles code-assistant's [`SessionCheckpoint`] from the loop checkpoint
/// plus [`crate::plugins::AgentAppState`], and forwards it to an
/// [`AgentStatePersistence`] backend.
pub struct SessionStateAdapter {
    inner: Box<dyn AgentStatePersistence>,
}

impl SessionStateAdapter {
    pub fn new(inner: Box<dyn AgentStatePersistence>) -> Self {
        Self { inner }
    }
}

impl CheckpointPersistence for SessionStateAdapter {
    fn commit(
        &mut self,
        checkpoint: &AgentCheckpoint<'_>,
        extensions: &(dyn std::any::Any + Send),
    ) -> Result<()> {
        let state = crate::plugins::AgentAppState::of_ref(extensions);
        let changed_executions = checkpoint
            .changed_executions
            .iter()
            .map(|execution| execution.serialize())
            .collect::<Result<_>>()?;

        self.inner.commit_checkpoint(SessionCheckpoint {
            session_id: checkpoint.session_id,
            name: &state.session_name,
            changed_nodes: &checkpoint.changed_nodes,
            active_path: checkpoint.active_path,
            next_node_id: checkpoint.next_node_id,
            changed_executions,
            plan: &state.plan,
            active_skills: &state.active_skills,
            next_request_id: checkpoint.next_request_id,
        })
    }
}

/// Discards checkpoints. For agents whose conversation is not a session of
/// its own (sub-agents) and for tests that do not look at persistence.
pub struct NoOpStatePersistence;

impl AgentStatePersistence for NoOpStatePersistence {
    fn commit_checkpoint(&mut self, _: SessionCheckpoint<'_>) -> Result<()> {
        Ok(())
    }
}

/// Commits checkpoints through the session manager, which owns the session
/// entry on disk and the active instance.
pub struct SessionStatePersistence {
    session_manager: Arc<Mutex<SessionManager>>,
}

impl SessionStatePersistence {
    pub fn new(session_manager: Arc<Mutex<SessionManager>>) -> Self {
        Self { session_manager }
    }
}

impl AgentStatePersistence for SessionStatePersistence {
    fn commit_checkpoint(&mut self, checkpoint: SessionCheckpoint<'_>) -> Result<()> {
        // The agent loop is synchronous at this point but runs on the tokio
        // runtime; block in place instead of holding an async lock across it.
        let mut session_manager = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.session_manager.lock())
        });
        session_manager.commit_checkpoint(checkpoint)
    }
}
