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
    /// Optional callback to update SessionInstance in memory
    update_callback: Option<
        Box<
            dyn Fn(
                    Vec<Message>,
                    Vec<ToolExecution>,
                    WorkingMemory,
                    Option<PathBuf>,
                    Option<String>,
                ) -> Result<()>
                + Send
                + Sync,
        >,
    >,
}

impl SessionManagerStatePersistence {
    pub fn new(session_manager: LegacySessionManager, tool_mode: ToolMode) -> Self {
        Self {
            session_manager,
            tool_mode,
            update_callback: None,
        }
    }

    /// Create with callback to update SessionInstance
    pub fn with_update_callback<F>(
        session_manager: LegacySessionManager,
        tool_mode: ToolMode,
        callback: F,
    ) -> Self
    where
        F: Fn(
                Vec<Message>,
                Vec<ToolExecution>,
                WorkingMemory,
                Option<PathBuf>,
                Option<String>,
            ) -> Result<()>
            + Send
            + Sync
            + 'static,
    {
        Self {
            session_manager,
            tool_mode,
            update_callback: Some(Box::new(callback)),
        }
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
            let _session_id = self
                .session_manager
                .create_session(task_name, self.tool_mode)?;
        }

        // First: Call update callback to update SessionInstance if available
        if let Some(ref callback) = self.update_callback {
            callback(
                messages.clone(),
                tool_executions.clone(),
                working_memory.clone(),
                init_path.clone(),
                initial_project.clone(),
            )?;
        }

        // Second: Save to persistence as before
        self.session_manager.save_session(
            messages,
            tool_executions,
            working_memory,
            init_path,
            initial_project,
        )
    }
}
