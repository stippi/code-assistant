use anyhow::Result;
use llm::Message;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::agent::types::ToolExecution;
use crate::persistence::{ChatSession, SerializedToolExecution};
use crate::session::SessionManager;
use crate::types::{ToolSyntax, WorkingMemory};

use std::time::SystemTime;
use tracing::{debug, info};

/// Trait for persisting agent state
/// This abstracts away the storage mechanism from the Agent implementation
pub trait AgentStatePersistence: Send + Sync {
    /// Save the current agent state
    fn save_agent_state(
        &mut self,
        name: String,
        messages: Vec<Message>,
        tool_executions: Vec<ToolExecution>,
        working_memory: WorkingMemory,
        init_path: Option<PathBuf>,
        initial_project: Option<String>,
        next_request_id: u64,
    ) -> Result<()>;
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
    fn save_agent_state(
        &mut self,
        _name: String,
        messages: Vec<Message>,
        tool_executions: Vec<ToolExecution>,
        working_memory: WorkingMemory,
        _init_path: Option<PathBuf>,
        _initial_project: Option<String>,
        _next_request_id: u64,
    ) -> Result<()> {
        self.save_count += 1;
        self.last_saved_messages = Some(messages);
        self.last_saved_tool_executions = Some(tool_executions);
        self.last_saved_working_memory = Some(working_memory);
        Ok(())
    }
}

/// Session-specific wrapper that implements AgentStatePersistence
/// This allows agents to save state to a specific session without the SessionManager
/// needing to track a single "current" session (which would break concurrent agents)
pub struct SessionStatePersistence {
    session_manager: Arc<Mutex<SessionManager>>,
    session_id: String,
}

impl SessionStatePersistence {
    pub fn new(session_manager: Arc<Mutex<SessionManager>>, session_id: String) -> Self {
        Self {
            session_manager,
            session_id,
        }
    }
}

impl AgentStatePersistence for SessionStatePersistence {
    fn save_agent_state(
        &mut self,
        name: String,
        messages: Vec<Message>,
        tool_executions: Vec<ToolExecution>,
        working_memory: WorkingMemory,
        init_path: Option<PathBuf>,
        initial_project: Option<String>,
        next_request_id: u64,
    ) -> Result<()> {
        let mut session_manager = self
            .session_manager
            .lock()
            .map_err(|_| anyhow::anyhow!("Failed to lock session manager"))?;

        session_manager.save_session_state(
            &self.session_id,
            name,
            messages,
            tool_executions,
            working_memory,
            init_path,
            initial_project,
            next_request_id,
        )
    }
}

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
    pub fn has_saved_state(&self) -> bool {
        self.state_file_path.exists()
    }
}

impl AgentStatePersistence for FileStatePersistence {
    fn save_agent_state(
        &mut self,
        name: String,
        messages: Vec<Message>,
        tool_executions: Vec<ToolExecution>,
        working_memory: WorkingMemory,
        init_path: Option<PathBuf>,
        initial_project: Option<String>,
        next_request_id: u64,
    ) -> Result<()> {
        debug!("Saving agent state to {}", self.state_file_path.display());

        // Convert tool executions to serialized form
        let serialized_executions: Result<Vec<SerializedToolExecution>> =
            tool_executions.iter().map(|te| te.serialize()).collect();

        let serialized_executions = serialized_executions?;

        // Create a ChatSession with the current state
        let session = ChatSession {
            id: "terminal-session".to_string(),
            name: name,
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
            messages,
            tool_executions: serialized_executions,
            working_memory,
            init_path,
            initial_project,
            tool_syntax: self.tool_syntax,
            use_diff_blocks: self.use_diff_blocks,
            next_request_id,
        };

        // Save to file
        let json = serde_json::to_string_pretty(&session)?;
        std::fs::write(&self.state_file_path, json)?;

        debug!("Agent state saved successfully");
        Ok(())
    }
}
