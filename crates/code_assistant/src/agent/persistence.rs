use anyhow::Result;
use llm::Message;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::agent::types::ToolExecution;
use crate::session::SessionManager;
use crate::types::WorkingMemory;

/// Trait for persisting agent state
/// This abstracts away the storage mechanism from the Agent implementation
pub trait AgentStatePersistence: Send + Sync {
    /// Save the current agent state
    fn save_agent_state(
        &mut self,
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
            messages,
            tool_executions,
            working_memory,
            init_path,
            initial_project,
            next_request_id,
        )
    }
}

