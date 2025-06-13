use anyhow::Result;
use llm::Message;
use std::path::PathBuf;
use std::time::SystemTime;

use crate::agent::ToolExecution;
use crate::persistence::{generate_session_id, ChatSession, FileStatePersistence};
use crate::types::{ToolMode, WorkingMemory};

// New session management architecture
pub mod instance;
pub mod multi_manager;

// New main session manager (V2)
pub use multi_manager::{AgentConfig, SessionManager};

/// Legacy session manager (V1) - kept for compatibility with state_storage.rs
pub struct LegacySessionManager {
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

impl LegacySessionManager {
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
        let session_id = self
            .current_session_id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No active session"))?;

        let mut session = self
            .persistence
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

    /// Get the ID of the currently active session
    pub fn current_session_id(&self) -> Option<&str> {
        self.current_session_id.as_deref()
    }

    /// Set the current session without loading it
    pub fn set_current_session(&mut self, session_id: String) {
        self.current_session_id = Some(session_id);
    }
}
