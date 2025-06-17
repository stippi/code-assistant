use anyhow::Result;
use llm::Message;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::agent::ToolExecution;
use crate::config::ProjectManager;
use crate::persistence::{generate_session_id, ChatMetadata, ChatSession, FileStatePersistence};
use crate::session::instance::SessionInstance;
use crate::types::{ToolMode, WorkingMemory};
use crate::ui::UserInterface;
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
            next_request_id: 1,
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
            request_id: None,
        };
        session_instance.add_message(user_msg.clone());

        // Generate message ID for this interaction
        let _message_id = session_instance.get_last_message_id();
        session_instance.start_streaming(_message_id.clone());

        // Get references for agent
        let _fragment_buffer = session_instance.get_fragment_buffer();
        let _is_ui_active = Arc::new(Mutex::new(session_instance.is_ui_active()));

        // Create a session-specific state storage wrapper
        // This allows the agent to save to the correct session without requiring
        // the SessionManager to track a single "current" session
        let session_manager_ref = Arc::new(Mutex::new(SessionManager::new(
            self.persistence.clone(),
            self.agent_config.clone(),
        )));

        let state_storage = Box::new(crate::agent::persistence::SessionStatePersistence::new(
            session_manager_ref,
            session_id.to_string(),
        ));

        let mut agent = crate::agent::Agent::new(
            llm_provider,
            self.agent_config.tool_mode,
            project_manager,
            command_executor,
            session_instance.create_proxy_ui(ui.clone()),
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
            next_request_id: Some(session_instance.session.next_request_id),
        };

        agent.load_from_session_state(session_state).await?;

        // Spawn the agent task - same as UI message handling
        let session_id_clone = session_id.to_string();

        let task_handle = tokio::spawn(async move {
            tracing::info!("🚀 V2: Starting agent for session {}", session_id_clone);
            // Run the agent once for this message (same as UI messages)
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

    /// Get the latest session ID for auto-resuming
    pub fn get_latest_session_id(&self) -> Result<Option<String>> {
        self.persistence.get_latest_session_id()
    }

    /// Save agent state to a specific session
    pub fn save_session_state(
        &mut self,
        session_id: &str,
        messages: Vec<Message>,
        tool_executions: Vec<ToolExecution>,
        working_memory: WorkingMemory,
        init_path: Option<PathBuf>,
        initial_project: Option<String>,
        next_request_id: u64,
    ) -> Result<()> {
        let mut session = self
            .persistence
            .load_chat_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

        // Update session with current state
        session.messages = messages;
        session.tool_executions = tool_executions
            .into_iter()
            .map(|te| te.serialize())
            .collect::<Result<Vec<_>>>()?;
        session.working_memory = working_memory;
        session.init_path = init_path;
        session.initial_project = initial_project;
        session.next_request_id = next_request_id;
        session.updated_at = SystemTime::now();

        self.persistence.save_chat_session(&session)?;

        // Update active session instance if it exists
        if let Some(instance) = self.active_sessions.get_mut(session_id) {
            instance.session = session;
        }

        Ok(())
    }
}
