use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

#[cfg(test)]
use crate::agent::ToolExecution;
#[cfg(test)]
use crate::types::WorkingMemory;
#[cfg(test)]
use llm::Message;

use crate::persistence::{ChatSession, SerializedToolExecution};
use crate::session::SessionManager;
use crate::types::ToolSyntax;

use std::time::SystemTime;
use tracing::{debug, info};

use crate::session::SessionState;

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
    pub last_saved_working_memory: Option<WorkingMemory>,
}

#[cfg(test)]
impl MockStatePersistence {
    pub fn new() -> Self {
        Self {
            save_count: 0,
            last_saved_messages: None,
            last_saved_tool_executions: None,
            last_saved_working_memory: None,
        }
    }
}

#[cfg(test)]
impl AgentStatePersistence for MockStatePersistence {
    fn save_agent_state(&mut self, state: SessionState) -> Result<()> {
        self.save_count += 1;
        self.last_saved_messages = Some(state.messages);
        self.last_saved_tool_executions = Some(state.tool_executions);
        self.last_saved_working_memory = Some(state.working_memory);
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
    tool_syntax: ToolSyntax,
    use_diff_blocks: bool,
}

impl FileStatePersistence {
    #[allow(dead_code)]
    pub fn new(working_dir: &Path, tool_syntax: ToolSyntax, use_diff_blocks: bool) -> Self {
        let state_file_path = working_dir.join(STATE_FILE);
        info!("Using state file: {}", state_file_path.display());
        Self {
            state_file_path,
            tool_syntax,
            use_diff_blocks,
        }
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
        let session: ChatSession = serde_json::from_str(&json)?;

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
        let serialized_executions: Result<Vec<SerializedToolExecution>> = state
            .tool_executions
            .iter()
            .map(|te| te.serialize())
            .collect();

        let serialized_executions = serialized_executions?;

        // Create a ChatSession with the current state
        let session = ChatSession {
            id: state.session_id,
            name: state.name,
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
            messages: state.messages,
            tool_executions: serialized_executions,
            working_memory: state.working_memory,
            init_path: state.init_path,
            initial_project: state.initial_project,
            tool_syntax: self.tool_syntax,
            use_diff_blocks: self.use_diff_blocks,
            next_request_id: state.next_request_id.unwrap_or(0),
            llm_config: state.llm_config,
        };

        // Save to file
        let json = serde_json::to_string_pretty(&session)?;
        std::fs::write(&self.state_file_path, json)?;

        debug!("Agent state saved successfully");
        Ok(())
    }
}
