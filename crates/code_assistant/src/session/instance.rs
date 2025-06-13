use anyhow::Result;
use llm::Message;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

// Agent instances are created on-demand, no need to import
use crate::persistence::ChatSession;
use crate::ui::gpui::elements::MessageRole;
use crate::ui::gpui::ui_events::{MessageData, UiEvent};
use crate::ui::streaming::create_stream_processor;
use crate::ui::{DisplayFragment, UIError, UIMessage, UserInterface};
use async_trait::async_trait;

/// Represents a single session instance with its own agent and state
pub struct SessionInstance {
    /// The session data (messages, metadata, etc.)
    pub session: ChatSession,

    // Agent instances are created on-demand and moved into tokio tasks
    // We only track the task handle, not the agent itself
    /// Task handle for the running agent (None if not running)
    pub task_handle: Option<JoinHandle<Result<()>>>,

    /// Buffer for DisplayFragments from the current streaming message
    /// This allows UI to connect mid-streaming and see buffered content
    pub fragment_buffer: Arc<Mutex<VecDeque<DisplayFragment>>>,

    /// Whether this session is currently streaming
    pub is_streaming: bool,

    /// Whether this session is currently connected to the UI
    /// (only the active session should be true)
    pub is_ui_active: Arc<Mutex<bool>>,

    /// The ID of the currently streaming message (if any)
    pub streaming_message_id: Option<String>,

    /// Whether this session's agent completed successfully
    pub agent_completed: bool,

    /// Error from the last agent run (if any)
    pub last_agent_error: Option<String>,
}

impl SessionInstance {
    /// Create a new session instance
    pub fn new(session: ChatSession) -> Self {
        Self {
            session,
            task_handle: None,
            fragment_buffer: Arc::new(Mutex::new(VecDeque::new())),
            is_streaming: false,
            is_ui_active: Arc::new(Mutex::new(false)),
            streaming_message_id: None,
            agent_completed: false,
            last_agent_error: None,
        }
    }

    /// Check if the agent is currently running
    pub fn is_agent_running(&self) -> bool {
        self.task_handle.is_some() && !self.agent_completed
    }

    /// Check if task is finished (synchronous, no await)
    pub fn is_task_finished(&self) -> bool {
        if let Some(handle) = &self.task_handle {
            handle.is_finished()
        } else {
            true // No task means finished
        }
    }

    /// Check if the task handle is finished
    pub async fn check_task_completion(&mut self) -> Result<()> {
        if let Some(handle) = &mut self.task_handle {
            if handle.is_finished() {
                // Take the handle to get the result
                let handle = self.task_handle.take().unwrap();
                match handle.await {
                    Ok(agent_result) => {
                        self.agent_completed = true;
                        match agent_result {
                            Ok(_) => {
                                self.last_agent_error = None;
                            }
                            Err(e) => {
                                self.last_agent_error = Some(e.to_string());
                            }
                        }
                    }
                    Err(join_error) => {
                        self.agent_completed = true;
                        self.last_agent_error = Some(format!("Task join error: {}", join_error));
                    }
                }
                self.is_streaming = false;
                self.streaming_message_id = None;
            }
        }
        Ok(())
    }

    /// Get all buffered fragments and optionally clear the buffer
    pub fn get_buffered_fragments(&self, clear: bool) -> Vec<DisplayFragment> {
        if let Ok(mut buffer) = self.fragment_buffer.lock() {
            let fragments: Vec<_> = buffer.iter().cloned().collect();
            if clear {
                buffer.clear();
            }
            fragments
        } else {
            Vec::new()
        }
    }

    /// Clear the fragment buffer
    pub fn clear_fragment_buffer(&self) {
        if let Ok(mut buffer) = self.fragment_buffer.lock() {
            buffer.clear();
        }
    }

    /// Start streaming a new message
    pub fn start_streaming(&mut self, message_id: String) {
        self.is_streaming = true;
        self.streaming_message_id = Some(message_id);
        self.clear_fragment_buffer();
        self.agent_completed = false;
        self.last_agent_error = None;
    }

    /// Stop streaming
    pub fn stop_streaming(&mut self) {
        self.is_streaming = false;
        self.streaming_message_id = None;
    }

    /// Terminate the running agent
    pub fn terminate_agent(&mut self) {
        if let Some(handle) = self.task_handle.take() {
            handle.abort();
            self.is_streaming = false;
            self.streaming_message_id = None;
            self.agent_completed = true;
        }
    }

    /// Get the session ID
    pub fn session_id(&self) -> &str {
        &self.session.id
    }

    /// Get the session name
    pub fn session_name(&self) -> &str {
        &self.session.name
    }

    /// Add a message to the session
    pub fn add_message(&mut self, message: Message) {
        self.session.messages.push(message);
        self.session.updated_at = std::time::SystemTime::now();
    }

    /// Get all messages in the session
    pub fn messages(&self) -> &[Message] {
        &self.session.messages
    }

    /// Update the complete session state from agent
    /// This keeps the SessionInstance synchronized with Agent state changes
    pub fn update_session_state(
        &mut self,
        messages: Vec<llm::Message>,
        tool_executions: Vec<crate::agent::ToolExecution>,
        working_memory: crate::types::WorkingMemory,
        init_path: Option<std::path::PathBuf>,
        initial_project: Option<String>,
    ) -> anyhow::Result<()> {
        // Update all session fields
        self.session.messages = messages;
        self.session.tool_executions = tool_executions
            .into_iter()
            .map(|te| te.serialize())
            .collect::<anyhow::Result<Vec<_>>>()?;
        self.session.working_memory = working_memory;
        self.session.init_path = init_path;
        self.session.initial_project = initial_project;
        self.session.updated_at = std::time::SystemTime::now();

        Ok(())
    }

    /// Reload session data from persistence
    /// This ensures SessionInstance has the latest state even if agents have made changes
    pub fn reload_from_persistence(
        &mut self,
        persistence: &crate::persistence::FileStatePersistence,
    ) -> anyhow::Result<()> {
        if let Some(session) = persistence.load_chat_session(&self.session.id)? {
            tracing::debug!("Reloading session {} from persistence", self.session.id);
            self.session = session;
        }
        Ok(())
    }

    /// Get the last message ID for streaming identification
    pub fn get_last_message_id(&self) -> String {
        format!("msg_{}_{}", self.session.id, self.session.messages.len())
    }

    /// Set UI active state for this session
    pub fn set_ui_active(&mut self, active: bool) {
        if let Ok(mut ui_active) = self.is_ui_active.lock() {
            *ui_active = active;
        }
    }

    /// Check if this session is currently connected to the UI
    pub fn is_ui_active(&self) -> bool {
        self.is_ui_active
            .lock()
            .map(|active| *active)
            .unwrap_or(false)
    }

    /// Get fragment buffer reference for agent access
    pub fn get_fragment_buffer(&self) -> Arc<Mutex<VecDeque<DisplayFragment>>> {
        self.fragment_buffer.clone()
    }

    /// Create a ProxyUI for this session that handles fragment buffering
    pub fn create_proxy_ui(
        &self,
        real_ui: Arc<Box<dyn UserInterface>>,
    ) -> Arc<Box<dyn UserInterface>> {
        Arc::new(Box::new(ProxyUI::new(
            real_ui,
            self.fragment_buffer.clone(),
            self.is_ui_active.clone(),
        )))
    }

    /// Generate UI events for connecting to this session
    /// Returns both the session messages and any buffered fragments from current streaming
    pub fn generate_session_connect_events(&self) -> Result<Vec<UiEvent>, anyhow::Error> {
        let mut events = Vec::new();

        // Always use SetMessages regardless of whether session is empty or not
        let messages_data = self.convert_messages_to_ui_data(self.session.tool_mode)?;
        let tool_results = self.convert_tool_executions_to_ui_data()?;

        events.push(UiEvent::SetMessages {
            messages: messages_data,
            session_id: Some(self.session.id.clone()),
            tool_results,
        });

        // Second event: Load buffered fragments if currently streaming
        if self.is_streaming {
            let buffered_fragments = self.get_buffered_fragments(false); // Don't clear buffer
            if !buffered_fragments.is_empty() {
                events.push(UiEvent::LoadSessionFragments {
                    fragments: buffered_fragments,
                    session_id: self.session.id.clone(),
                });
            }
        }

        Ok(events)
    }

    /// Convert session messages to UI MessageData format
    fn convert_messages_to_ui_data(
        &self,
        tool_mode: crate::types::ToolMode,
    ) -> Result<Vec<MessageData>, anyhow::Error> {
        // Create dummy UI for stream processor
        struct DummyUI;
        #[async_trait::async_trait]
        impl crate::ui::UserInterface for DummyUI {
            async fn display(
                &self,
                _message: crate::ui::UIMessage,
            ) -> Result<(), crate::ui::UIError> {
                Ok(())
            }
            async fn get_input(&self) -> Result<String, crate::ui::UIError> {
                Ok("".to_string())
            }
            fn display_fragment(
                &self,
                _fragment: &crate::ui::DisplayFragment,
            ) -> Result<(), crate::ui::UIError> {
                Ok(())
            }
            async fn update_memory(
                &self,
                _memory: &crate::types::WorkingMemory,
            ) -> Result<(), crate::ui::UIError> {
                Ok(())
            }
            async fn update_tool_status(
                &self,
                _tool_id: &str,
                _status: crate::ui::ToolStatus,
                _message: Option<String>,
                _output: Option<String>,
            ) -> Result<(), crate::ui::UIError> {
                Ok(())
            }
            async fn begin_llm_request(&self) -> Result<u64, crate::ui::UIError> {
                Ok(0)
            }
            async fn end_llm_request(
                &self,
                _request_id: u64,
                _cancelled: bool,
            ) -> Result<(), crate::ui::UIError> {
                Ok(())
            }
            fn should_streaming_continue(&self) -> bool {
                true
            }
        }

        let dummy_ui = std::sync::Arc::new(Box::new(DummyUI) as Box<dyn crate::ui::UserInterface>);
        let mut processor = create_stream_processor(tool_mode, dummy_ui);

        let mut messages_data = Vec::new();

        tracing::warn!(
            "preparing {} messages for event",
            self.session.messages.len()
        );

        for message in &self.session.messages {
            // Filter out tool-result user messages (they have empty content or structured content)
            if message.role == llm::MessageRole::User {
                match &message.content {
                    llm::MessageContent::Text(text) if text.trim().is_empty() => {
                        // Skip empty user messages (tool results in XML mode)
                        continue;
                    }
                    llm::MessageContent::Structured(_) => {
                        // Skip structured user messages (tool results)
                        continue;
                    }
                    _ => {
                        // This is a real user message, process it
                    }
                }
            }

            match processor.extract_fragments_from_message(message) {
                Ok(fragments) => {
                    let role = match message.role {
                        llm::MessageRole::User => MessageRole::User,
                        llm::MessageRole::Assistant => MessageRole::Assistant,
                    };
                    messages_data.push(MessageData { role, fragments });
                }
                Err(e) => {
                    tracing::error!("Failed to extract fragments from message: {}", e);
                    // Continue with other messages even if one fails
                }
            }
        }

        tracing::warn!("prepared {} message data for event", messages_data.len());

        Ok(messages_data)
    }

    /// Convert tool executions to UI tool result data
    fn convert_tool_executions_to_ui_data(
        &self,
    ) -> Result<Vec<crate::ui::gpui::ui_events::ToolResultData>, anyhow::Error> {
        use crate::tools::core::ResourcesTracker;

        let mut tool_results = Vec::new();
        let mut resources_tracker = ResourcesTracker::new();

        for serialized_execution in &self.session.tool_executions {
            // Deserialize the tool execution
            let execution = serialized_execution.deserialize()?;

            // Generate status and output from result
            let success = execution.result.is_success();
            let status = if success {
                crate::ui::ToolStatus::Success
            } else {
                crate::ui::ToolStatus::Error
            };

            let short_output = execution.result.as_render().status();
            let output = execution.result.as_render().render(&mut resources_tracker);

            tool_results.push(crate::ui::gpui::ui_events::ToolResultData {
                tool_id: execution.tool_request.id,
                status,
                message: Some(short_output),
                output: Some(output),
            });
        }

        Ok(tool_results)
    }
}

/// ProxyUI that buffers fragments and conditionally forwards to real UI
struct ProxyUI {
    real_ui: Arc<Box<dyn UserInterface>>,
    fragment_buffer: Arc<Mutex<VecDeque<DisplayFragment>>>,
    is_session_active: Arc<Mutex<bool>>,
}

impl ProxyUI {
    pub fn new(
        real_ui: Arc<Box<dyn UserInterface>>,
        fragment_buffer: Arc<Mutex<VecDeque<DisplayFragment>>>,
        is_session_active: Arc<Mutex<bool>>,
    ) -> Self {
        Self {
            real_ui,
            fragment_buffer,
            is_session_active,
        }
    }

    /// Check if this session is currently active
    fn is_active(&self) -> bool {
        self.is_session_active
            .lock()
            .map(|active| *active)
            .unwrap_or(false)
    }
}

#[async_trait]
impl UserInterface for ProxyUI {
    async fn display(&self, message: UIMessage) -> Result<(), UIError> {
        if self.is_active() {
            self.real_ui.display(message).await
        } else {
            Ok(()) // NOP if session not active
        }
    }

    async fn get_input(&self) -> Result<String, UIError> {
        if self.is_active() {
            self.real_ui.get_input().await
        } else {
            Ok(String::new()) // Return empty string if session not active
        }
    }

    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError> {
        // Always buffer fragments
        if let Ok(mut buffer) = self.fragment_buffer.lock() {
            buffer.push_back(fragment.clone());

            // Keep buffer size reasonable
            while buffer.len() > 1000 {
                buffer.pop_front();
            }
        }

        // Only forward to real UI if session is active
        if self.is_active() {
            self.real_ui.display_fragment(fragment)
        } else {
            Ok(())
        }
    }

    async fn update_memory(&self, memory: &crate::types::WorkingMemory) -> Result<(), UIError> {
        if self.is_active() {
            self.real_ui.update_memory(memory).await
        } else {
            Ok(()) // NOP if session not active
        }
    }

    async fn update_tool_status(
        &self,
        tool_id: &str,
        status: crate::ui::ToolStatus,
        message: Option<String>,
        output: Option<String>,
    ) -> Result<(), UIError> {
        if self.is_active() {
            self.real_ui
                .update_tool_status(tool_id, status, message, output)
                .await
        } else {
            Ok(()) // NOP if session not active
        }
    }

    async fn begin_llm_request(&self) -> Result<u64, UIError> {
        // Clear fragment buffer at start of new LLM request
        if let Ok(mut buffer) = self.fragment_buffer.lock() {
            buffer.clear();
        }

        if self.is_active() {
            self.real_ui.begin_llm_request().await
        } else {
            Ok(0) // Return dummy request ID if session not active
        }
    }

    async fn end_llm_request(&self, request_id: u64, cancelled: bool) -> Result<(), UIError> {
        // Clear fragment buffer when LLM request ends - fragments are now part of message history
        if let Ok(mut buffer) = self.fragment_buffer.lock() {
            buffer.clear();
        }
        
        if self.is_active() {
            self.real_ui.end_llm_request(request_id, cancelled).await
        } else {
            Ok(()) // NOP if session not active
        }
    }

    fn should_streaming_continue(&self) -> bool {
        if self.is_active() {
            self.real_ui.should_streaming_continue()
        } else {
            true // Don't interrupt streaming if session becomes inactive
        }
    }
}
