use anyhow::Result;
use llm::Message;
use std::path::PathBuf;
use std::time::SystemTime;

use crate::agent::ToolExecution;
use crate::persistence::{ChatMetadata, ChatSession, FileStatePersistence, generate_session_id};
use crate::types::{ToolMode, WorkingMemory};

// New session management architecture
pub mod instance;
pub mod multi_manager;

pub use instance::SessionInstance;
pub use multi_manager::{MultiSessionManager, AgentConfig, SessionSwitchData};

/// Manages chat sessions independently from the Agent
pub struct SessionManager {
    persistence: FileStatePersistence,
    current_session_id: Option<String>,
}

/// State data needed to restore an agent session
#[derive(Debug)]
pub struct SessionState {
    pub messages: Vec<Message>,
    pub tool_executions: Vec<ToolExecution>,
    pub working_memory: WorkingMemory,
    pub init_path: Option<PathBuf>,
    pub initial_project: Option<String>,
}

impl SessionManager {
    pub fn new(persistence: FileStatePersistence) -> Self {
        Self {
            persistence,
            current_session_id: None,
        }
    }

    /// Create a new chat session and return its ID
    pub fn create_session(&mut self, name: Option<String>, tool_mode: ToolMode) -> Result<String> {
        let session_id = generate_session_id();
        let session_name = name.unwrap_or_else(|| format!("Chat {}", &session_id[5..13])); // Show part of ID

        let session = ChatSession {
            id: session_id.clone(),
            name: session_name,
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
            messages: Vec::new(),
            tool_executions: Vec::new(),
            working_memory: WorkingMemory::default(),
            init_path: None,
            initial_project: None,
            tool_mode,
        };

        self.persistence.save_chat_session(&session)?;
        self.current_session_id = Some(session_id.clone());

        Ok(session_id)
    }

    /// Save current agent state to the active session
    pub fn save_session(
        &mut self,
        messages: Vec<Message>,
        tool_executions: Vec<ToolExecution>,
        working_memory: WorkingMemory,
        init_path: Option<PathBuf>,
        initial_project: Option<String>,
    ) -> Result<()> {
        let session_id = self.current_session_id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No active session"))?;

        let mut session = self.persistence
            .load_chat_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session not found"))?;

        // Update session with current state
        session.messages = messages;
        session.tool_executions = tool_executions
            .into_iter()
            .map(|te| te.serialize())
            .collect::<Result<Vec<_>>>()?;
        session.working_memory = working_memory;
        session.init_path = init_path;
        session.initial_project = initial_project;
        session.updated_at = SystemTime::now();

        self.persistence.save_chat_session(&session)?;
        Ok(())
    }

    /// Load a session and return its state for agent restoration
    pub fn load_session(&mut self, session_id: &str) -> Result<SessionState> {
        let session = self.persistence
            .load_chat_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

        self.current_session_id = Some(session_id.to_string());

        let tool_executions = session.tool_executions
            .into_iter()
            .map(|se| se.deserialize())
            .collect::<Result<Vec<_>>>()?;

        Ok(SessionState {
            messages: session.messages,
            tool_executions,
            working_memory: session.working_memory,
            init_path: session.init_path,
            initial_project: session.initial_project,
        })
    }

    /// List all available chat sessions
    pub fn list_sessions(&self) -> Result<Vec<ChatMetadata>> {
        self.persistence.list_chat_sessions()
    }

    /// Delete a chat session
    pub fn delete_session(&mut self, session_id: &str) -> Result<()> {
        // If we're deleting the current session, clear the current session ID
        if self.current_session_id.as_deref() == Some(session_id) {
            self.current_session_id = None;
        }

        self.persistence.delete_chat_session(session_id)
    }

    /// Get the ID of the currently active session
    pub fn current_session_id(&self) -> Option<&str> {
        self.current_session_id.as_deref()
    }

    /// Set the current session without loading it
    pub fn set_current_session(&mut self, session_id: String) {
        self.current_session_id = Some(session_id);
    }

    /// Get the latest session ID for auto-resuming
    pub fn get_latest_session_id(&self) -> Result<Option<String>> {
        self.persistence.get_latest_session_id()
    }
}
