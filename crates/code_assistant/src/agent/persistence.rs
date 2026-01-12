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

/// Trait for persisting agent state
/// This abstracts away the storage mechanism from the Agent implementation
pub trait AgentStatePersistence: Send + Sync {
    /// Save the current agent state
    fn save_agent_state(&mut self, state: SessionState) -> Result<()>;
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

        // Save to file
        let json = serde_json::to_string_pretty(&session)?;
        std::fs::write(&self.state_file_path, json)?;

        debug!("Agent state saved successfully");
        Ok(())
    }
}
