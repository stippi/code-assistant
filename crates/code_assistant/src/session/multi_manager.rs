use anyhow::Result;
use llm::Message;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use async_trait::async_trait;

use crate::agent::Agent;
use crate::persistence::{ChatMetadata, ChatSession, FileStatePersistence, generate_session_id};
use crate::session::instance::SessionInstance;
use crate::types::{ToolMode, WorkingMemory};
use crate::ui::{DisplayFragment, UserInterface};
use crate::config::ProjectManager;
use crate::utils::CommandExecutor;
use llm::LLMProvider;

/// The new SessionManager that manages multiple active sessions with on-demand agents
pub struct MultiSessionManager {
    /// Persistence layer for saving/loading sessions
    persistence: FileStatePersistence,

    /// Active session instances (session_id -> SessionInstance)
    /// These can have running agents
    active_sessions: Arc<Mutex<HashMap<String, SessionInstance>>>,

    /// The currently UI-active session ID
    active_session_id: Arc<Mutex<Option<String>>>,

    /// Shared configuration for creating agents
    agent_config: AgentConfig,
}

/// Configuration needed to create new agents
#[derive(Clone)]
pub struct AgentConfig {
    pub tool_mode: ToolMode,
    pub init_path: Option<PathBuf>,
    pub initial_project: Option<String>,
}

impl MultiSessionManager {
    /// Create a new MultiSessionManager
    pub fn new(persistence: FileStatePersistence, agent_config: AgentConfig) -> Self {
        Self {
            persistence,
            active_sessions: Arc::new(Mutex::new(HashMap::new())),
            active_session_id: Arc::new(Mutex::new(None)),
            agent_config,
        }
    }

    /// Create a new session and return its ID
    pub async fn create_session(&mut self, name: Option<String>) -> Result<String> {
        let session_id = generate_session_id();
        let session_name = name.unwrap_or_else(|| format!("Chat {}", &session_id[5..13]));

        let session = ChatSession {
            id: session_id.clone(),
            name: session_name,
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
            messages: Vec::new(),
            tool_executions: Vec::new(),
            working_memory: WorkingMemory::default(),
            init_path: self.agent_config.init_path.clone(),
            initial_project: self.agent_config.initial_project.clone(),
        };

        // Save to persistence
        self.persistence.save_chat_session(&session)?;

        // Create session instance
        let instance = SessionInstance::new(session);

        // Add to active sessions
        {
            let mut active_sessions = self.active_sessions.lock().unwrap();
            active_sessions.insert(session_id.clone(), instance);
        }

        Ok(session_id)
    }

    /// Load a session from persistence and make it active
    pub async fn load_session(&mut self, session_id: &str) -> Result<Vec<Message>> {
        // Load from persistence
        let session = self.persistence
            .load_chat_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

        let messages = session.messages.clone();

        // Create session instance
        let instance = SessionInstance::new(session);

        // Add to active sessions
        {
            let mut active_sessions = self.active_sessions.lock().unwrap();
            active_sessions.insert(session_id.to_string(), instance);
        }

        Ok(messages)
    }

    /// Set the UI-active session
    pub async fn set_active_session(&mut self, session_id: String) -> Result<()> {
        // Ensure the session exists in active sessions
        {
            let active_sessions = self.active_sessions.lock().unwrap();
            if !active_sessions.contains_key(&session_id) {
                // Try to load it
                drop(active_sessions); // Release lock before calling load_session
                self.load_session(&session_id).await?;
            }
        }

        // Set as active
        {
            let mut active_id = self.active_session_id.lock().unwrap();
            *active_id = Some(session_id);
        }

        Ok(())
    }

    /// Get the currently UI-active session ID
    pub fn get_active_session_id(&self) -> Option<String> {
        self.active_session_id.lock().unwrap().clone()
    }

    /// Start an agent for a session with a user message
    /// This is the key method - agents run on-demand for specific messages
    /// For now, simplified version without complex threading
    pub async fn start_agent_for_message(
        &mut self,
        session_id: &str,
        user_message: String,
        _llm_provider: Box<dyn LLMProvider>,
        _project_manager: Box<dyn ProjectManager>,
        _command_executor: Box<dyn CommandExecutor>,
        _ui: Arc<Box<dyn UserInterface>>,
    ) -> Result<()> {
        // Add user message to session
        {
            let mut active_sessions = self.active_sessions.lock().unwrap();
            let session_instance = active_sessions
                .get_mut(session_id)
                .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

            let user_msg = Message {
                role: llm::MessageRole::User,
                content: llm::MessageContent::Text(user_message.clone()),
            };
            session_instance.add_message(user_msg);

            let message_id = session_instance.get_last_message_id();
            session_instance.start_streaming(message_id);
        }

        // TODO: Implement actual agent spawning
        // For now, just acknowledge the message was received
        tracing::info!("User message received for session {}: {}", session_id, user_message);

        Ok(())
    }

    /// Get buffered fragments for the active session (for UI connection mid-streaming)
    pub fn get_active_session_fragments(&self, clear_buffer: bool) -> Vec<DisplayFragment> {
        let active_session_id = self.get_active_session_id();
        if let Some(session_id) = active_session_id {
            let active_sessions = self.active_sessions.lock().unwrap();
            if let Some(session_instance) = active_sessions.get(&session_id) {
                return session_instance.get_buffered_fragments(clear_buffer);
            }
        }
        Vec::new()
    }

    /// Check completion status of all running agents
    pub async fn check_agent_completions(&mut self) -> Result<Vec<String>> {
        let mut completed_sessions = Vec::new();

        {
            let mut active_sessions = self.active_sessions.lock().unwrap();
            for (session_id, session_instance) in active_sessions.iter_mut() {
                session_instance.check_task_completion().await?;
                if session_instance.agent_completed {
                    completed_sessions.push(session_id.clone());
                }
            }
        }

        Ok(completed_sessions)
    }

    /// List all available sessions (both active and persisted)
    pub fn list_all_sessions(&self) -> Result<Vec<ChatMetadata>> {
        self.persistence.list_chat_sessions()
    }

    /// List currently active sessions
    pub fn list_active_sessions(&self) -> Vec<String> {
        let active_sessions = self.active_sessions.lock().unwrap();
        active_sessions.keys().cloned().collect()
    }

    /// Check if a session is currently streaming
    pub fn is_session_streaming(&self, session_id: &str) -> bool {
        let active_sessions = self.active_sessions.lock().unwrap();
        active_sessions.get(session_id)
            .map(|instance| instance.is_streaming)
            .unwrap_or(false)
    }

    /// Delete a session
    pub async fn delete_session(&mut self, session_id: &str) -> Result<()> {
        // Remove from active sessions
        {
            let mut active_sessions = self.active_sessions.lock().unwrap();
            if let Some(mut session_instance) = active_sessions.remove(session_id) {
                session_instance.terminate_agent().await;
            }
        }

        // Clear active session if it was the deleted one
        {
            let mut active_id = self.active_session_id.lock().unwrap();
            if active_id.as_deref() == Some(session_id) {
                *active_id = None;
            }
        }

        // Delete from persistence
        self.persistence.delete_chat_session(session_id)?;

        Ok(())
    }

    /// Save a session to persistence
    pub async fn save_session(&mut self, session_id: &str) -> Result<()> {
        let active_sessions = self.active_sessions.lock().unwrap();
        if let Some(session_instance) = active_sessions.get(session_id) {
            self.persistence.save_chat_session(&session_instance.session)?;
        }
        Ok(())
    }

    /// Get the latest session ID for auto-resuming
    pub fn get_latest_session_id(&self) -> Result<Option<String>> {
        self.persistence.get_latest_session_id()
    }
}

// For now, let's skip the BufferingUI implementation as it has complex trait issues
// The core architecture can work without it initially
