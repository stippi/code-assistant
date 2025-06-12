use anyhow::Result;
use async_trait::async_trait;
use llm::Message;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::agent::{Agent, SessionManagerStatePersistence};
use crate::config::ProjectManager;
use crate::persistence::{generate_session_id, ChatMetadata, ChatSession, FileStatePersistence};
use crate::session::instance::SessionInstance;
use crate::types::{ToolMode, WorkingMemory};
use crate::ui::{DisplayFragment, UserInterface};
use crate::utils::CommandExecutor;
use llm::LLMProvider;

/// The main SessionManager that manages multiple active sessions with on-demand agents
pub struct SessionManager {
    /// Persistence layer for saving/loading sessions
    persistence: FileStatePersistence,

    /// Active session instances (session_id -> SessionInstance)
    /// These can have running agents
    active_sessions: HashMap<String, SessionInstance>,

    /// The currently UI-active session ID
    active_session_id: Option<String>,

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

impl SessionManager {
    /// Create a new SessionManager
    pub fn new(persistence: FileStatePersistence, agent_config: AgentConfig) -> Self {
        Self {
            persistence,
            active_sessions: HashMap::new(),
            active_session_id: None,
            agent_config,
        }
    }

    /// Create a new session and return its ID
    pub fn create_session(&mut self, name: Option<String>) -> Result<String> {
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
            tool_mode: self.agent_config.tool_mode,
        };

        // Save to persistence
        self.persistence.save_chat_session(&session)?;

        // Create session instance
        let instance = SessionInstance::new(session);

        // Add to active sessions
        self.active_sessions.insert(session_id.clone(), instance);

        Ok(session_id)
    }

    /// Load a session from persistence and make it active
    pub fn load_session(&mut self, session_id: &str) -> Result<Vec<Message>> {
        // Load from persistence
        let session = self
            .persistence
            .load_chat_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

        let messages = session.messages.clone();

        // Create session instance
        let instance = SessionInstance::new(session);

        // Add to active sessions
        self.active_sessions
            .insert(session_id.to_string(), instance);

        Ok(messages)
    }

    /// Set the UI-active session and return events for UI update
    pub async fn set_active_session(
        &mut self,
        session_id: String,
    ) -> Result<Vec<crate::ui::gpui::ui_events::UiEvent>> {
        // Deactivate old session
        if let Some(old_id) = &self.active_session_id {
            if old_id != &session_id {
                if let Some(old_session) = self.active_sessions.get_mut(old_id) {
                    old_session.set_ui_active(false);
                }
            }
        }

        // Check if session exists
        let session_exists = self.active_sessions.contains_key(&session_id);

        // Load session if it doesn't exist
        if !session_exists {
            self.load_session(&session_id)?;
        }

        // Activate new session and generate UI events
        let session_instance = self.active_sessions.get_mut(&session_id).unwrap();
        session_instance.set_ui_active(true);

        // Generate UI events for connecting to this session
        let ui_events = session_instance.generate_session_connect_events()?;

        // Set as active
        self.active_session_id = Some(session_id);

        Ok(ui_events)
    }

    /// Get the currently UI-active session ID
    pub fn get_active_session_id(&self) -> Option<String> {
        self.active_session_id.clone()
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
        let session_instance = self
            .active_sessions
            .get_mut(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

        // Add user message to session
        let user_msg = Message {
            role: llm::MessageRole::User,
            content: llm::MessageContent::Text(user_message.clone()),
        };
        session_instance.add_message(user_msg);

        // Generate message ID for this interaction
        let _message_id = session_instance.get_last_message_id();
        session_instance.start_streaming(_message_id.clone());

        // Get references for agent
        let _fragment_buffer = session_instance.get_fragment_buffer();
        let _is_ui_active = Arc::new(Mutex::new(session_instance.is_ui_active()));

        // Create a new legacy session manager for this agent
        let mut session_manager_for_agent =
            crate::session::LegacySessionManager::new(self.persistence.clone());
        
        // CRITICAL: Set the current session ID so the agent doesn't create a new session
        session_manager_for_agent.set_current_session(session_id.to_string());
        
        let state_storage = Box::new(SessionManagerStatePersistence::new(
            session_manager_for_agent,
            self.agent_config.tool_mode,
        ));

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
        let session_instance = self.active_sessions.get(session_id).unwrap();
        let session_state = crate::session::SessionState {
            messages: session_instance.messages().to_vec(),
            tool_executions: session_instance
                .session
                .tool_executions
                .iter()
                .map(|se| se.deserialize())
                .collect::<Result<Vec<_>>>()?,
            working_memory: session_instance.session.working_memory.clone(),
            init_path: session_instance.session.init_path.clone(),
            initial_project: session_instance.session.initial_project.clone(),
        };

        agent.load_from_session_state(session_state).await?;

        // Spawn the agent task
        let session_id_clone = session_id.to_string();

        let task_handle = tokio::spawn(async move {
            tracing::info!("🚀 V2: Starting agent for session {}", session_id_clone);
            // Run the agent once for this message
            let result = agent.run_single_iteration().await;

            tracing::info!("✅ V2: Agent completed for session {}", session_id_clone);
            result
        });

        // Store the task handle (but not the agent, as it's moved into the task)
        let session_instance = self
            .active_sessions
            .get_mut(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

        session_instance.task_handle = Some(task_handle);

        Ok(())
    }

    /// Get buffered fragments for the active session (for UI connection mid-streaming)
    pub fn get_active_session_fragments(&self, clear_buffer: bool) -> Vec<DisplayFragment> {
        if let Some(session_id) = &self.active_session_id {
            if let Some(session_instance) = self.active_sessions.get(session_id) {
                return session_instance.get_buffered_fragments(clear_buffer);
            }
        }
        Vec::new()
    }

    /// Check completion status of all running agents
    pub async fn check_agent_completions(&mut self) -> Result<Vec<String>> {
        let mut completed_sessions = Vec::new();

        // First, collect session IDs that have finished tasks
        let finished_session_ids: Vec<String> = self
            .active_sessions
            .iter()
            .filter(|(_, session_instance)| session_instance.is_task_finished())
            .map(|(session_id, _)| session_id.clone())
            .collect();

        // Now handle the finished tasks one by one
        for session_id in finished_session_ids {
            // Check if the session still exists and needs completion processing
            if let Some(session_instance) = self.active_sessions.get(&session_id) {
                if session_instance.is_task_finished() && !session_instance.agent_completed {
                    // Extract the task handle
                    let task_handle = self
                        .active_sessions
                        .get_mut(&session_id)
                        .and_then(|instance| instance.task_handle.take());

                    // Process the completed task
                    if let Some(handle) = task_handle {
                        if handle.is_finished() {
                            let completion_result = match handle.await {
                                Ok(agent_result) => match agent_result {
                                    Ok(_) => (true, None),
                                    Err(e) => (true, Some(e.to_string())),
                                },
                                Err(join_error) => {
                                    (true, Some(format!("Task join error: {}", join_error)))
                                }
                            };

                            // Update session state based on completion result
                            let (completed, error) = completion_result;
                            if completed {
                                if let Some(session_instance) =
                                    self.active_sessions.get_mut(&session_id)
                                {
                                    session_instance.agent_completed = true;
                                    session_instance.last_agent_error = error;
                                    session_instance.is_streaming = false;
                                    session_instance.streaming_message_id = None;
                                }
                                completed_sessions.push(session_id);
                            }
                        }
                    }
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
        self.active_sessions.keys().cloned().collect()
    }

    /// Check if a session is currently streaming
    pub fn is_session_streaming(&self, session_id: &str) -> bool {
        self.active_sessions
            .get(session_id)
            .map(|instance| instance.is_streaming)
            .unwrap_or(false)
    }

    /// Delete a session
    pub fn delete_session(&mut self, session_id: &str) -> Result<()> {
        // Remove from active sessions
        if let Some(mut session_instance) = self.active_sessions.remove(session_id) {
            session_instance.terminate_agent();
        }

        // Clear active session if it was the deleted one
        if self.active_session_id.as_deref() == Some(session_id) {
            self.active_session_id = None;
        }

        // Delete from persistence
        self.persistence.delete_chat_session(session_id)?;

        Ok(())
    }

    /// Save a session to persistence
    pub fn save_session(&mut self, session_id: &str) -> Result<()> {
        if let Some(session_instance) = self.active_sessions.get(session_id) {
            self.persistence
                .save_chat_session(&session_instance.session)?;
        }
        Ok(())
    }

    /// Get the latest session ID for auto-resuming
    pub fn get_latest_session_id(&self) -> Result<Option<String>> {
        self.persistence.get_latest_session_id()
    }

    /// Helper method to send UI events for session connection
    /// This method sends multiple UI events over the channel to properly connect to a session
    pub async fn send_session_connect_events(
        ui_events: Vec<crate::ui::gpui::ui_events::UiEvent>,
        ui_event_sender: &async_channel::Sender<crate::ui::UIMessage>,
    ) -> Result<()> {
        for event in ui_events {
            ui_event_sender
                .send(crate::ui::UIMessage::UiEvent(event))
                .await
                .map_err(|e| anyhow::anyhow!("Failed to send UI event: {}", e))?;
        }
        Ok(())
    }
}

// For now, let's skip the BufferingUI implementation as it has complex trait issues
// The core architecture can work without it initially
