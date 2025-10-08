use anyhow::Result;
use llm::Message;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::Mutex;

use crate::agent::{Agent, AgentComponents, AgentOptions};
use crate::config::ProjectManager;
use crate::persistence::{
    generate_session_id, ChatMetadata, ChatSession, FileSessionPersistence, LlmSessionConfig,
};
use crate::session::instance::SessionInstance;
use crate::session::SessionState;
use crate::types::{ToolSyntax, WorkingMemory};
use crate::ui::UserInterface;
use crate::utils::CommandExecutor;
use llm::LLMProvider;
use tracing::{debug, error};

/// The main SessionManager that manages multiple active sessions with on-demand agents
pub struct SessionManager {
    /// Persistence layer for saving/loading sessions
    persistence: FileSessionPersistence,

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
    pub tool_syntax: ToolSyntax,
    pub init_path: Option<PathBuf>,
    pub initial_project: String,
    pub use_diff_blocks: bool,
}

/// Resources required to launch an agent for a user message.
pub struct AgentLaunchResources {
    pub llm_provider: Box<dyn LLMProvider>,
    pub project_manager: Box<dyn ProjectManager>,
    pub command_executor: Box<dyn CommandExecutor>,
    pub ui: Arc<dyn UserInterface>,
    pub model_hint: Option<String>,
    pub session_llm_config: Option<LlmSessionConfig>,
}

impl SessionManager {
    /// Create a new SessionManager
    pub fn new(persistence: FileSessionPersistence, agent_config: AgentConfig) -> Self {
        Self {
            persistence,
            active_sessions: HashMap::new(),
            active_session_id: None,
            agent_config,
        }
    }

    /// Create a new session and return its ID
    pub fn create_session(&mut self, name: Option<String>) -> Result<String> {
        self.create_session_with_config(name, None)
    }

    /// Create a new session with optional LLM config and return its ID
    pub fn create_session_with_config(
        &mut self,
        name: Option<String>,
        llm_config: Option<LlmSessionConfig>,
    ) -> Result<String> {
        let session_id = generate_session_id();
        let session_name = name.unwrap_or_default(); // Empty string if no name provided

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
            tool_syntax: self.agent_config.tool_syntax,
            use_diff_blocks: self.agent_config.use_diff_blocks,
            next_request_id: 1,
            llm_config,
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
    ) -> Result<Vec<crate::ui::ui_events::UiEvent>> {
        // Deactivate old session
        if let Some(old_id) = &self.active_session_id {
            if old_id != &session_id {
                if let Some(old_session) = self.active_sessions.get_mut(old_id) {
                    old_session.set_ui_connected(false);
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
        session_instance.set_ui_connected(true);

        // Reload session from persistence to get latest state
        // This ensures we see any changes made by agents since session was loaded
        session_instance.reload_from_persistence(&self.persistence)?;

        // Generate UI events for connecting to this session
        let ui_events = session_instance.generate_session_connect_events()?;

        // Set as active
        self.active_session_id = Some(session_id);

        Ok(ui_events)
    }

    /// Start an agent for a session with a user message
    /// This is the key method - agents run on-demand for specific messages
    pub async fn start_agent_for_message(
        &mut self,
        session_id: &str,
        content_blocks: Vec<llm::ContentBlock>,
        resources: AgentLaunchResources,
    ) -> Result<()> {
        let AgentLaunchResources {
            llm_provider,
            project_manager,
            command_executor,
            ui,
            model_hint,
            session_llm_config,
        } = resources;
        // Prepare session - need to scope the mutable borrow carefully
        let (
            tool_syntax,
            use_diff_blocks,
            init_path,
            proxy_ui,
            session_state,
            activity_state_ref,
            pending_message_ref,
        ) = {
            let session_instance = self
                .active_sessions
                .get_mut(session_id)
                .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

            // Make sure the session instance is not stale
            session_instance.reload_from_persistence(&self.persistence)?;

            // Add structured user message to session
            let user_msg = Message {
                role: llm::MessageRole::User,
                content: llm::MessageContent::Structured(content_blocks),
                request_id: None,
                usage: None,
            };
            session_instance.add_message(user_msg);

            // Clone all needed data to avoid borrowing conflicts
            let name = session_instance.session.name.clone();
            let tool_syntax = session_instance.session.tool_syntax;
            let use_diff_blocks = session_instance.session.use_diff_blocks;
            let init_path = session_instance.session.init_path.clone();
            let proxy_ui = session_instance.create_proxy_ui(ui.clone());
            let activity_state_ref = session_instance.activity_state.clone();
            let pending_message_ref = session_instance.pending_message.clone();

            let resolved_session_llm_config = session_llm_config
                .clone()
                .or_else(|| session_instance.session.llm_config.clone());

            if resolved_session_llm_config.is_some() {
                session_instance.session.llm_config = resolved_session_llm_config.clone();
            }

            let session_state = crate::session::SessionState {
                session_id: session_id.to_string(),
                name,
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
                next_request_id: Some(session_instance.session.next_request_id),
                llm_config: resolved_session_llm_config.clone(),
            };

            // Set activity state
            session_instance
                .set_activity_state(crate::session::instance::SessionActivityState::AgentRunning);

            (
                tool_syntax,
                use_diff_blocks,
                init_path,
                proxy_ui,
                session_state,
                activity_state_ref,
                pending_message_ref,
            )
        };

        // Now save the session state with the user message (outside the borrow scope)
        self.save_session_state(session_state.clone())?;

        // Broadcast the initial state change
        let _ = ui
            .send_event(crate::ui::UiEvent::UpdateSessionActivityState {
                session_id: session_id.to_string(),
                activity_state: crate::session::instance::SessionActivityState::AgentRunning,
            })
            .await;

        // Create agent components
        let session_manager_ref = Arc::new(Mutex::new(SessionManager::new(
            self.persistence.clone(),
            self.agent_config.clone(),
        )));

        let state_storage = Box::new(crate::agent::persistence::SessionStatePersistence::new(
            session_manager_ref,
        ));

        let components = AgentComponents {
            llm_provider,
            project_manager,
            command_executor,
            ui: proxy_ui,
            state_persistence: state_storage,
        };

        let options = AgentOptions {
            tool_syntax,
            init_path,
            model_hint,
        };

        let mut agent = Agent::new(components, options);

        // Configure diff blocks format based on session setting
        if use_diff_blocks {
            agent.enable_diff_blocks();
        }

        // Set the shared pending message reference
        agent.set_pending_message_ref(pending_message_ref);

        // Load the session state into the agent
        agent.load_from_session_state(session_state).await?;

        // Spawn the agent task
        let session_id_clone = session_id.to_string();
        let ui_clone = ui.clone();

        let task_handle = tokio::spawn(async move {
            debug!("Starting agent for session {}", session_id_clone);
            let result = agent.run_single_iteration().await;

            // Always set session state back to Idle when agent task ends
            debug!(
                "Agent task ending for session {}, setting state to Idle",
                session_id_clone
            );
            if let Ok(mut state) = activity_state_ref.lock() {
                *state = crate::session::instance::SessionActivityState::Idle;
            }

            // Always broadcast the state change to UI
            let send_result = ui_clone
                .send_event(crate::ui::UiEvent::UpdateSessionActivityState {
                    session_id: session_id_clone.clone(),
                    activity_state: crate::session::instance::SessionActivityState::Idle,
                })
                .await;

            if let Err(e) = send_result {
                debug!(
                    "Failed to send UpdateSessionActivityState event for session {}: {}",
                    session_id_clone, e
                );
            } else {
                debug!(
                    "Successfully sent UpdateSessionActivityState(Idle) event for session {}",
                    session_id_clone
                );
            }

            // Log the completion with detailed error information if failed
            match &result {
                Ok(()) => {
                    debug!(
                        "Agent completed successfully for session {}",
                        session_id_clone
                    );
                }
                Err(e) => {
                    error!("Agent failed for session {}: {}", session_id_clone, e);
                    // Also log the full error chain for debugging
                    debug!(
                        "Agent error chain for session {}: {:?}",
                        session_id_clone, e
                    );

                    // Send error to UI for user notification
                    let error_message = format!("Agent error: {e}");
                    if let Err(ui_error) = ui_clone
                        .send_event(crate::ui::UiEvent::DisplayError {
                            message: error_message,
                        })
                        .await
                    {
                        error!(
                            "Failed to send error to UI for session {}: {}",
                            session_id_clone, ui_error
                        );
                    }
                }
            }
            result
        });

        // Update the task handle for this session
        if let Some(session_instance) = self.active_sessions.get_mut(session_id) {
            session_instance.task_handle = Some(task_handle);
        }

        Ok(())
    }

    /// List all available sessions (both active and persisted)
    pub fn list_all_sessions(&self) -> Result<Vec<ChatMetadata>> {
        self.persistence.list_chat_sessions()
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

    /// Get a session instance by ID
    pub fn get_session(&self, session_id: &str) -> Option<&SessionInstance> {
        self.active_sessions.get(session_id)
    }

    /// Get a mutable session instance by ID
    pub fn get_session_mut(&mut self, session_id: &str) -> Option<&mut SessionInstance> {
        self.active_sessions.get_mut(session_id)
    }

    /// Get the LLM config for a session, if any
    pub fn get_session_llm_config(&self, session_id: &str) -> Result<Option<LlmSessionConfig>> {
        if let Some(instance) = self.active_sessions.get(session_id) {
            Ok(instance.session.llm_config.clone())
        } else {
            // Load from persistence
            match self.persistence.load_chat_session(session_id)? {
                Some(session) => Ok(session.llm_config),
                None => Ok(None),
            }
        }
    }

    /// Get the latest session ID for auto-resuming
    pub fn get_latest_session_id(&self) -> Result<Option<String>> {
        self.persistence.get_latest_session_id()
    }

    /// Get metadata for a specific session
    #[allow(dead_code)]
    pub fn get_session_metadata(&self, session_id: &str) -> Result<Option<ChatMetadata>> {
        self.persistence.get_chat_session_metadata(session_id)
    }

    /// Save agent state to a specific session
    pub fn save_session_state(&mut self, state: SessionState) -> Result<()> {
        let mut session = self
            .persistence
            .load_chat_session(&state.session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {}", state.session_id))?;

        // Update session with current state
        session.name = state.name;
        session.messages = state.messages;
        session.tool_executions = state
            .tool_executions
            .into_iter()
            .map(|te| te.serialize())
            .collect::<Result<Vec<_>>>()?;
        session.working_memory = state.working_memory;
        session.init_path = state.init_path;
        session.initial_project = state.initial_project;
        session.next_request_id = state.next_request_id.unwrap_or(0);
        session.updated_at = SystemTime::now();

        self.persistence.save_chat_session(&session)?;

        // Update active session instance if it exists
        if let Some(instance) = self.active_sessions.get_mut(&state.session_id) {
            instance.session = session;
        }

        Ok(())
    }

    /// Get the current context size for the active session
    /// Returns the input tokens + cache reads from the most recent assistant message
    #[allow(dead_code)]
    pub fn get_current_context_size(&self) -> u32 {
        if let Some(session_id) = &self.active_session_id {
            if let Some(session_instance) = self.active_sessions.get(session_id) {
                return session_instance.get_current_context_size();
            }
        }
        0
    }

    /// Calculate total usage for the active session
    #[allow(dead_code)]
    pub fn get_total_session_usage(&self) -> llm::Usage {
        if let Some(session_id) = &self.active_session_id {
            if let Some(session_instance) = self.active_sessions.get(session_id) {
                return session_instance.calculate_total_usage();
            }
        }
        llm::Usage::zero()
    }

    /// Get usage data for a specific session
    #[allow(dead_code)]
    pub fn get_session_usage(&self, session_id: &str) -> Option<(u32, llm::Usage)> {
        if let Some(session_instance) = self.active_sessions.get(session_id) {
            let context_size = session_instance.get_current_context_size();
            let total_usage = session_instance.calculate_total_usage();
            Some((context_size, total_usage))
        } else {
            None
        }
    }

    /// Queue a user message for a running agent session
    pub fn queue_user_message(&mut self, session_id: &str, message: String) -> Result<()> {
        // Get the active session instance and update shared pending message
        let session_instance = self
            .active_sessions
            .get(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

        // Update the shared pending message
        let mut pending = session_instance.pending_message.lock().unwrap();
        match pending.as_mut() {
            Some(existing) => {
                // Append to existing message with newline separator
                if !existing.is_empty() && !existing.ends_with('\n') {
                    existing.push('\n');
                }
                existing.push_str(&message);
            }
            None => {
                // Set as new pending message
                *pending = Some(message);
            }
        }

        Ok(())
    }

    /// Check for completed agent tasks and handle their results
    /// This should be called periodically to catch agent failures
    #[allow(dead_code)]
    pub async fn check_completed_tasks(&mut self) -> Result<()> {
        let mut completed_sessions = Vec::new();

        // Check all active sessions for completed tasks
        for (session_id, session_instance) in &mut self.active_sessions {
            if let Some(ref mut task_handle) = session_instance.task_handle {
                // Check if the task is finished (non-blocking)
                if task_handle.is_finished() {
                    // Task is complete, get the result
                    match task_handle.await {
                        Ok(Ok(())) => {
                            debug!(
                                "Agent task completed successfully for session {}",
                                session_id
                            );
                        }
                        Ok(Err(e)) => {
                            error!("Agent task failed for session {}: {}", session_id, e);
                            // Log the full error chain for debugging
                            debug!("Agent error chain for session {}: {:?}", session_id, e);
                            // Note: Error display is handled in the agent task itself
                        }
                        Err(join_error) => {
                            if join_error.is_cancelled() {
                                debug!("Agent task was cancelled for session {}", session_id);
                            } else if join_error.is_panic() {
                                error!(
                                    "Agent task panicked for session {}: {:?}",
                                    session_id, join_error
                                );
                            } else {
                                error!(
                                    "Agent task join error for session {}: {}",
                                    session_id, join_error
                                );
                            }
                        }
                    }
                    completed_sessions.push(session_id.clone());
                }
            }
        }

        // Clear task handles for completed sessions
        for session_id in completed_sessions {
            if let Some(session_instance) = self.active_sessions.get_mut(&session_id) {
                session_instance.task_handle = None;
            }
        }

        Ok(())
    }

    /// Queue structured content (text + attachments) for a running agent session
    pub fn queue_structured_user_message(
        &mut self,
        session_id: &str,
        content_blocks: Vec<llm::ContentBlock>,
    ) -> Result<()> {
        // For now, convert structured content to text representation for pending messages
        // In the future, we could extend pending messages to support structured content
        let text_representation = content_blocks
            .iter()
            .filter_map(|block| match block {
                llm::ContentBlock::Text { text, .. } => Some(text.clone()),
                llm::ContentBlock::Image { media_type, .. } => {
                    Some(format!("[Image: {media_type}]"))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        self.queue_user_message(session_id, text_representation)
    }

    /// Get and clear pending message for editing
    pub fn request_pending_message_for_edit(&mut self, session_id: &str) -> Result<Option<String>> {
        // Get the active session instance and take the pending message
        let session_instance = self
            .active_sessions
            .get(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

        let mut pending = session_instance.pending_message.lock().unwrap();
        Ok(pending.take())
    }

    /// Get current pending message without clearing it
    pub fn get_pending_message(&self, session_id: &str) -> Result<Option<String>> {
        let session_instance = self
            .active_sessions
            .get(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

        let pending = session_instance.pending_message.lock().unwrap();
        Ok(pending.clone())
    }
}
