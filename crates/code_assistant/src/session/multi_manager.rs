use anyhow::Result;
use llm::Message;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use async_trait::async_trait;

use crate::agent::{Agent, SessionManagerStatePersistence};
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

/// Data returned when switching sessions
pub struct SessionSwitchData {
    pub session_id: String,
    pub messages: Vec<Message>,
    pub buffered_fragments: Vec<DisplayFragment>,
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

    /// Set the UI-active session and return data for UI update
    pub async fn set_active_session(&mut self, session_id: String) -> Result<SessionSwitchData> {
        // Deactivate old session
        {
            let active_id = self.active_session_id.lock().unwrap();
            if let Some(old_id) = active_id.as_ref() {
                if old_id != &session_id {
                    let mut active_sessions = self.active_sessions.lock().unwrap();
                    if let Some(old_session) = active_sessions.get_mut(old_id) {
                        old_session.set_ui_active(false);
                    }
                }
            }
        }

        // Check if session exists
        let session_exists = {
            let active_sessions = self.active_sessions.lock().unwrap();
            active_sessions.contains_key(&session_id)
        };

        // Load session if it doesn't exist
        if !session_exists {
            self.load_session(&session_id).await?;
        }

        // Activate new session and collect data
        let session_data = {
            let mut active_sessions = self.active_sessions.lock().unwrap();
            let session_instance = active_sessions.get_mut(&session_id).unwrap();

            session_instance.set_ui_active(true);

            SessionSwitchData {
                session_id: session_id.clone(),
                messages: session_instance.session.messages.clone(),
                buffered_fragments: if session_instance.is_streaming {
                    session_instance.get_buffered_fragments(false) // Don't clear buffer
                } else {
                    Vec::new() // No agent running = no buffered fragments
                }
            }
        };

        // Set as active
        {
            let mut active_id = self.active_session_id.lock().unwrap();
            *active_id = Some(session_id);
        }

        Ok(session_data)
    }

    /// Get the currently UI-active session ID
    pub fn get_active_session_id(&self) -> Option<String> {
        self.active_session_id.lock().unwrap().clone()
    }

    /// Start an agent for a session with a user message
    /// This is the key method - agents run on-demand for specific messages
    pub async fn start_agent_for_message(
        &mut self,
        session_id: &str,
        user_message: String,
        llm_provider: Box<dyn LLMProvider>,
        project_manager: Box<dyn ProjectManager>,
        command_executor: Box<dyn CommandExecutor>,
        ui: Arc<Box<dyn UserInterface>>,
    ) -> Result<()> {
        // Prepare session and get references for agent
        let (message_id, fragment_buffer, is_ui_active) = {
            let mut active_sessions = self.active_sessions.lock().unwrap();
            let session_instance = active_sessions
                .get_mut(session_id)
                .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

            // Add user message to session
            let user_msg = Message {
                role: llm::MessageRole::User,
                content: llm::MessageContent::Text(user_message.clone()),
            };
            session_instance.add_message(user_msg);

            // Generate message ID for this interaction
            let message_id = session_instance.get_last_message_id();
            session_instance.start_streaming(message_id.clone());

            // Get references for agent
            let fragment_buffer = session_instance.get_fragment_buffer();
            let is_ui_active = Arc::new(Mutex::new(session_instance.is_ui_active()));

            (message_id, fragment_buffer, is_ui_active)
        };

        // Create a new agent for this session
        let session_manager_for_agent = crate::session::SessionManager::new(self.persistence.clone());
        let state_storage = Box::new(SessionManagerStatePersistence::new(session_manager_for_agent));

        let mut agent = crate::agent::Agent::new(
            llm_provider,
            self.agent_config.tool_mode,
            project_manager,
            command_executor,
            ui.clone(),
            state_storage,
            self.agent_config.init_path.clone(),
        );

        // Load the session state into the agent
        let session_state = {
            let active_sessions = self.active_sessions.lock().unwrap();
            let session_instance = active_sessions.get(session_id).unwrap();

            crate::session::SessionState {
                messages: session_instance.messages().to_vec(),
                tool_executions: session_instance.session.tool_executions.iter().map(|se| se.deserialize()).collect::<Result<Vec<_>>>()?,
                working_memory: session_instance.session.working_memory.clone(),
                init_path: session_instance.session.init_path.clone(),
                initial_project: session_instance.session.initial_project.clone(),
            }
        };

        agent.load_from_session_state(session_state).await?;

        // Spawn the agent task
        let session_id_clone = session_id.to_string();
        let active_sessions_clone = self.active_sessions.clone();

        let task_handle = tokio::spawn(async move {
            tracing::info!("🚀 V2: Starting agent for session {}", session_id_clone);
            // Run the agent once for this message
            let result = agent.run_single_iteration().await;

            // Mark streaming as complete
            {
                let mut active_sessions = active_sessions_clone.lock().unwrap();
                if let Some(session_instance) = active_sessions.get_mut(&session_id_clone) {
                    session_instance.stop_streaming();
                }
            }

            tracing::info!("✅ V2: Agent completed for session {}", session_id_clone);
            result
        });

        // Store the task handle (but not the agent, as it's moved into the task)
        {
            let mut active_sessions = self.active_sessions.lock().unwrap();
            let session_instance = active_sessions
                .get_mut(session_id)
                .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

            session_instance.task_handle = Some(task_handle);
        }

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

        // First, collect session IDs that have finished tasks (synchronous check)
        let finished_session_ids: Vec<String> = {
            let active_sessions = self.active_sessions.lock().unwrap();
            active_sessions
                .iter()
                .filter(|(_, session_instance)| session_instance.is_task_finished())
                .map(|(session_id, _)| session_id.clone())
                .collect()
        };

        // Now handle the finished tasks one by one (avoiding lock across await)
        for session_id in finished_session_ids {
            // Check if the session still exists and needs completion processing
            let needs_processing = {
                let active_sessions = self.active_sessions.lock().unwrap();
                active_sessions.get(&session_id)
                    .map(|instance| instance.is_task_finished() && !instance.agent_completed)
                    .unwrap_or(false)
            };

            if needs_processing {
                // Process completion without holding the lock
                let completion_result = {
                    // Extract the task handle without holding the lock across await
                    let task_handle = {
                        let mut active_sessions = self.active_sessions.lock().unwrap();
                        if let Some(session_instance) = active_sessions.get_mut(&session_id) {
                            session_instance.task_handle.take()
                        } else {
                            None
                        }
                    }; // Lock released here

                    // Process the completed task outside the lock
                    if let Some(handle) = task_handle {
                        if handle.is_finished() {
                            match handle.await {
                                Ok(agent_result) => {
                                    match agent_result {
                                        Ok(_) => (true, None),
                                        Err(e) => (true, Some(e.to_string())),
                                    }
                                }
                                Err(join_error) => (true, Some(format!("Task join error: {}", join_error))),
                            }
                        } else {
                            (false, None)
                        }
                    } else {
                        (false, None)
                    }
                };

                // Update session state based on completion result
                let (completed, error) = completion_result;
                if completed {
                    let mut active_sessions = self.active_sessions.lock().unwrap();
                    if let Some(session_instance) = active_sessions.get_mut(&session_id) {
                        session_instance.agent_completed = true;
                        session_instance.last_agent_error = error;
                        session_instance.is_streaming = false;
                        session_instance.streaming_message_id = None;
                    }
                    completed_sessions.push(session_id);
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
