use anyhow::Result;
use llm::Message;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::Mutex;

use crate::agent::{Agent, AgentComponents, DefaultSubAgentRunner, SubAgentCancellationRegistry};
use crate::config::ProjectManager;
use crate::permissions::PermissionMediator;
use crate::persistence::{
    generate_session_id, ChatMetadata, ChatSession, FileSessionPersistence, SessionModelConfig,
};
use crate::session::instance::SessionInstance;
use crate::session::{SessionConfig, SessionState};
use crate::ui::ui_events::UiEvent;
use crate::ui::UserInterface;
use command_executor::{CommandExecutor, SandboxedCommandExecutor};
use llm::LLMProvider;
use sandbox::SandboxPolicy;
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

    /// Template configuration applied to each new session
    session_config_template: SessionConfig,

    /// Default model name to use when creating sessions
    default_model_name: String,
}

impl SessionManager {
    /// Create a new SessionManager
    pub fn new(
        persistence: FileSessionPersistence,
        session_config_template: SessionConfig,
        default_model_name: String,
    ) -> Self {
        Self {
            persistence,
            active_sessions: HashMap::new(),
            active_session_id: None,
            session_config_template,
            default_model_name,
        }
    }

    /// Create a new session and return its ID
    pub fn create_session(&mut self, name: Option<String>) -> Result<String> {
        // Always create sessions with a default model config
        let default_model_config = self.default_model_config();
        self.create_session_with_config(name, None, Some(default_model_config))
    }

    /// Get the default model configuration
    fn default_model_config(&self) -> SessionModelConfig {
        SessionModelConfig::new(self.default_model_name.clone())
    }

    /// Create a new session with optional model config and return its ID
    pub fn create_session_with_config(
        &mut self,
        name: Option<String>,
        session_config_override: Option<SessionConfig>,
        model_config: Option<SessionModelConfig>,
    ) -> Result<String> {
        let session_id = generate_session_id();
        let session_name = name.unwrap_or_default(); // Empty string if no name provided

        let session = ChatSession::new_empty(
            session_id.clone(),
            session_name,
            session_config_override.unwrap_or_else(|| self.session_config_template.clone()),
            model_config,
        );

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
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;

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

        // Ensure the session has a model configuration and capture it for UI update
        let mut needs_persist = false;
        {
            let session_instance = self.active_sessions.get_mut(&session_id).unwrap();
            session_instance.set_ui_connected(true);

            // Reload session from persistence to get latest state
            // This ensures we see any changes made by agents since session was loaded
            session_instance.reload_from_persistence(&self.persistence)?;
        }

        let model_name_for_event = {
            let existing_model_name = {
                let session_instance = self.active_sessions.get_mut(&session_id).unwrap();
                session_instance
                    .session
                    .model_config
                    .as_ref()
                    .map(|config| config.model_name.clone())
            };

            if let Some(model_name) = existing_model_name {
                model_name
            } else {
                let default_model_config = self.default_model_config();
                let model_name = default_model_config.model_name.clone();
                {
                    let session_instance = self.active_sessions.get_mut(&session_id).unwrap();
                    session_instance.session.model_config = Some(default_model_config.clone());
                }
                needs_persist = true;
                model_name
            }
        };

        // Generate UI events for connecting to this session
        let mut ui_events = {
            let session_instance = self.active_sessions.get_mut(&session_id).unwrap();
            session_instance.generate_session_connect_events()?
        };

        ui_events.push(UiEvent::UpdateCurrentModel {
            model_name: model_name_for_event.clone(),
        });

        let sandbox_policy_for_event = {
            let session_instance = self.active_sessions.get(&session_id).unwrap();
            session_instance.session.config.sandbox_policy.clone()
        };

        ui_events.push(UiEvent::UpdateSandboxPolicy {
            policy: sandbox_policy_for_event,
        });

        // Set as active
        self.active_session_id = Some(session_id.clone());

        // Persist session if we had to backfill a default model configuration
        if needs_persist {
            let session_snapshot = {
                let session_instance = self.active_sessions.get(&session_id).unwrap();
                session_instance.session.clone()
            };
            self.persistence.save_chat_session(&session_snapshot)?;
        }

        Ok(ui_events)
    }

    /// Start an agent for a session with a user message
    /// This is the key method - agents run on-demand for specific messages
    #[allow(clippy::too_many_arguments)]
    pub async fn start_agent_for_message(
        &mut self,
        session_id: &str,
        content_blocks: Vec<llm::ContentBlock>,
        llm_provider: Box<dyn LLMProvider>,
        project_manager: Box<dyn ProjectManager>,
        command_executor: Box<dyn CommandExecutor>,
        ui: Arc<dyn UserInterface>,
        permission_handler: Option<Arc<dyn PermissionMediator>>,
    ) -> Result<()> {
        // Prepare session - need to scope the mutable borrow carefully
        let (
            session_config,
            proxy_ui,
            session_state,
            activity_state_ref,
            pending_message_ref,
            sandbox_context,
        ) = {
            let session_instance = self
                .active_sessions
                .get_mut(session_id)
                .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;

            // Make sure the session instance is not stale
            session_instance.reload_from_persistence(&self.persistence)?;

            // Add structured user message to session
            session_instance.add_message(Message::new_user_content(content_blocks));

            // Clone all needed data to avoid borrowing conflicts
            let name = session_instance.session.name.clone();
            let session_config = session_instance.session.config.clone();
            let proxy_ui = session_instance.create_proxy_ui(ui.clone());
            let activity_state_ref = session_instance.activity_state.clone();
            let pending_message_ref = session_instance.pending_message.clone();

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
                plan: session_instance.session.plan.clone(),
                config: session_config.clone(),
                next_request_id: Some(session_instance.session.next_request_id),
                model_config: session_instance.session.model_config.clone(),
            };

            // Set activity state
            session_instance
                .set_activity_state(crate::session::instance::SessionActivityState::AgentRunning);

            (
                session_config,
                proxy_ui,
                session_state,
                activity_state_ref,
                pending_message_ref,
                session_instance.sandbox_context.clone(),
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
            self.session_config_template.clone(),
            self.default_model_name.clone(),
        )));

        let state_storage = Box::new(crate::agent::persistence::SessionStatePersistence::new(
            session_manager_ref,
        ));

        let sandbox_context_clone = sandbox_context.clone();

        let command_executor: Box<dyn CommandExecutor> =
            if session_config.sandbox_policy.requires_restrictions() {
                Box::new(SandboxedCommandExecutor::new(
                    command_executor,
                    session_config.sandbox_policy.clone(),
                    Some(sandbox_context_clone.clone()),
                    Some(session_id.to_string()),
                ))
            } else {
                command_executor
            };

        let sandboxed_project_manager = Box::new(crate::config::SandboxAwareProjectManager::new(
            project_manager,
            sandbox_context_clone.clone(),
        ));

        // Get the cancellation registry from the session instance
        let sub_agent_cancellation_registry = self
            .active_sessions
            .get(session_id)
            .map(|s| s.sub_agent_cancellation_registry.clone())
            .unwrap_or_else(|| Arc::new(SubAgentCancellationRegistry::default()));

        // Create sub-agent runner
        let model_name_for_subagent = session_state
            .model_config
            .as_ref()
            .map(|c| c.model_name.clone())
            .unwrap_or_else(|| self.default_model_name.clone());
        let sub_agent_runner: Arc<dyn crate::agent::SubAgentRunner> =
            Arc::new(DefaultSubAgentRunner::new(
                model_name_for_subagent,
                session_config.clone(),
                sandbox_context_clone,
                sub_agent_cancellation_registry.clone(),
                proxy_ui.clone(),
                permission_handler.clone(),
            ));

        let components = AgentComponents {
            llm_provider,
            project_manager: sandboxed_project_manager,
            command_executor,
            ui: proxy_ui,
            state_persistence: state_storage,
            permission_handler,
            sub_agent_runner: Some(sub_agent_runner),
        };

        let mut agent = Agent::new(components, session_config.clone());

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

    /// Cancel a running sub-agent by its tool ID
    /// Returns Ok(true) if the sub-agent was found and cancelled,
    /// Ok(false) if the sub-agent was not found (may have already completed)
    pub fn cancel_sub_agent(&self, session_id: &str, tool_id: &str) -> Result<bool> {
        if let Some(session_instance) = self.active_sessions.get(session_id) {
            Ok(session_instance.cancel_sub_agent(tool_id))
        } else {
            Err(anyhow::anyhow!("Session not found: {}", session_id))
        }
    }

    /// Get a session instance by ID
    pub fn get_session(&self, session_id: &str) -> Option<&SessionInstance> {
        self.active_sessions.get(session_id)
    }

    /// Get a mutable session instance by ID
    pub fn get_session_mut(&mut self, session_id: &str) -> Option<&mut SessionInstance> {
        self.active_sessions.get_mut(session_id)
    }

    /// Get the model config for a session, if any
    pub fn get_session_model_config(&self, session_id: &str) -> Result<Option<SessionModelConfig>> {
        if let Some(instance) = self.active_sessions.get(session_id) {
            Ok(instance.session.model_config.clone())
        } else {
            // Load from persistence
            match self.persistence.load_chat_session(session_id)? {
                Some(mut session) => {
                    if let Some(config) = session.model_config.take() {
                        Ok(Some(config))
                    } else {
                        Ok(None)
                    }
                }
                None => Ok(None),
            }
        }
    }

    /// Update the persisted model config for a session
    pub fn set_session_model_config(
        &mut self,
        session_id: &str,
        model_config: Option<SessionModelConfig>,
    ) -> Result<()> {
        let mut session = self
            .persistence
            .load_chat_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;

        session.model_config = model_config.clone();
        self.persistence.save_chat_session(&session)?;

        if let Some(instance) = self.active_sessions.get_mut(session_id) {
            instance.session.model_config = model_config;
        }

        Ok(())
    }

    pub fn set_session_sandbox_policy(
        &mut self,
        session_id: &str,
        policy: SandboxPolicy,
    ) -> Result<()> {
        let mut session = self
            .persistence
            .load_chat_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;

        session.config.sandbox_policy = policy.clone();
        self.persistence.save_chat_session(&session)?;

        if let Some(instance) = self.active_sessions.get_mut(session_id) {
            instance.session.config.sandbox_policy = policy;
        }

        Ok(())
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
        session.plan = state.plan;
        session.config = state.config;
        session.model_config = state.model_config;
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
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;

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
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;

        let mut pending = session_instance.pending_message.lock().unwrap();
        Ok(pending.take())
    }

    /// Get current pending message without clearing it
    pub fn get_pending_message(&self, session_id: &str) -> Result<Option<String>> {
        let session_instance = self
            .active_sessions
            .get(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;

        let pending = session_instance.pending_message.lock().unwrap();
        Ok(pending.clone())
    }
}
