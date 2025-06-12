use anyhow::Result;
use llm::Message;
use std::path::PathBuf;

use crate::agent::types::ToolExecution;
use crate::session::LegacySessionManager;
use crate::types::{ToolMode, WorkingMemory};

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
    ) -> Result<()>;
}

/// Mock implementation for testing
pub struct MockStatePersistence {
    pub save_count: usize,
    pub last_saved_messages: Option<Vec<Message>>,
    pub last_saved_tool_executions: Option<Vec<ToolExecution>>,
    pub last_saved_working_memory: Option<WorkingMemory>,
}

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

impl AgentStatePersistence for MockStatePersistence {
    fn save_agent_state(
        &mut self,
        messages: Vec<Message>,
        tool_executions: Vec<ToolExecution>,
        working_memory: WorkingMemory,
        _init_path: Option<PathBuf>,
        _initial_project: Option<String>,
    ) -> Result<()> {
        self.save_count += 1;
        self.last_saved_messages = Some(messages);
        self.last_saved_tool_executions = Some(tool_executions);
        self.last_saved_working_memory = Some(working_memory);
        Ok(())
    }
}

/// Wrapper that implements AgentStatePersistence for LegacySessionManager
/// This allows existing V1 SessionManager code to work with the new Agent interface
pub struct SessionManagerStatePersistence {
    session_manager: LegacySessionManager,
    tool_mode: ToolMode,
}

impl SessionManagerStatePersistence {
    pub fn new(session_manager: LegacySessionManager, tool_mode: ToolMode) -> Self {
        Self { session_manager, tool_mode }
    }
    
    pub fn session_manager(&self) -> &LegacySessionManager {
        &self.session_manager
    }
    
    pub fn session_manager_mut(&mut self) -> &mut LegacySessionManager {
        &mut self.session_manager
    }
}

impl AgentStatePersistence for SessionManagerStatePersistence {
    fn save_agent_state(
        &mut self,
        messages: Vec<Message>,
        tool_executions: Vec<ToolExecution>,
        working_memory: WorkingMemory,
        init_path: Option<PathBuf>,
        initial_project: Option<String>,
    ) -> Result<()> {
        // Create a session if none exists (backward compatibility)
        if self.session_manager.current_session_id().is_none() {
            let task_name = if !working_memory.current_task.is_empty() {
                Some(working_memory.current_task.clone())
            } else {
                None
            };
            let _session_id = self.session_manager.create_session(task_name, self.tool_mode)?;
        }

        self.session_manager.save_session(
            messages,
            tool_executions,
            working_memory,
            init_path,
            initial_project,
        )
    }
}