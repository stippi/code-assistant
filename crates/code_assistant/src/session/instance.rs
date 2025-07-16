use anyhow::Result;
use llm::Message;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

// Agent instances are created on-demand, no need to import
use crate::persistence::ChatSession;
use crate::ui::gpui::elements::MessageRole;
use crate::ui::streaming::create_stream_processor;
use crate::ui::ui_events::{MessageData, UiEvent};
use crate::ui::{DisplayFragment, UIError, UserInterface};
use async_trait::async_trait;
use tracing::{debug, error, trace};

/// Represents the current activity state of a session
#[derive(Debug, Clone, PartialEq)]
pub enum SessionActivityState {
    /// No agent running, waiting for user input
    Idle,
    /// Agent loop is active (running tools, processing)
    AgentRunning,
    /// Agent sent LLM request, waiting for first streaming chunk
    WaitingForResponse,
    /// Agent is rate limited with countdown
    RateLimited { seconds_remaining: u64 },
}

impl Default for SessionActivityState {
    fn default() -> Self {
        SessionActivityState::Idle
    }
}

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

    /// Whether this session is currently connected to the UI
    pub is_ui_connected: Arc<Mutex<bool>>,

    /// Current activity state of this session
    pub activity_state: Arc<Mutex<SessionActivityState>>,
}

impl SessionInstance {
    /// Create a new session instance
    pub fn new(session: ChatSession) -> Self {
        Self {
            session,
            task_handle: None,
            fragment_buffer: Arc::new(Mutex::new(VecDeque::new())),
            is_ui_connected: Arc::new(Mutex::new(false)),
            activity_state: Arc::new(Mutex::new(SessionActivityState::Idle)),
        }
    }

    /// Get the current activity state
    pub fn get_activity_state(&self) -> SessionActivityState {
        self.activity_state.lock().unwrap().clone()
    }

    /// Set the activity state
    pub fn set_activity_state(&self, state: SessionActivityState) {
        *self.activity_state.lock().unwrap() = state;
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

    /// Terminate the running agent
    pub fn terminate_agent(&mut self) {
        if let Some(handle) = self.task_handle.take() {
            handle.abort();
            self.clear_fragment_buffer();
        }
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

    /// Get the current context size (input tokens + cache reads from most recent assistant message)
    /// This represents the total tokens being processed in the current LLM request
    #[allow(dead_code)]
    pub fn get_current_context_size(&self) -> u32 {
        // Find the most recent assistant message with usage data
        for message in self.session.messages.iter().rev() {
            if matches!(message.role, llm::MessageRole::Assistant) {
                if let Some(usage) = &message.usage {
                    return usage.input_tokens + usage.cache_read_input_tokens;
                }
            }
        }
        0
    }

    /// Calculate total usage across the entire session
    #[allow(dead_code)]
    pub fn calculate_total_usage(&self) -> llm::Usage {
        let mut total = llm::Usage::zero();

        for message in &self.session.messages {
            if let Some(usage) = &message.usage {
                total.input_tokens += usage.input_tokens;
                total.output_tokens += usage.output_tokens;
                total.cache_creation_input_tokens += usage.cache_creation_input_tokens;
                total.cache_read_input_tokens += usage.cache_read_input_tokens;
            }
        }

        total
    }

    /// Reload session data from persistence
    /// This ensures SessionInstance has the latest state even if agents have made changes
    pub fn reload_from_persistence(
        &mut self,
        persistence: &crate::persistence::FileSessionPersistence,
    ) -> anyhow::Result<()> {
        if let Some(session) = persistence.load_chat_session(&self.session.id)? {
            debug!("Reloading session {} from persistence", self.session.id);
            self.session = session;
        }
        Ok(())
    }

    /// Set UI active state for this session
    pub fn set_ui_connected(&mut self, connected: bool) {
        if let Ok(mut ui_connected) = self.is_ui_connected.lock() {
            *ui_connected = connected;
        }
    }

    /// Create a ProxyUI for this session that handles fragment buffering
    pub fn create_proxy_ui(
        &self,
        real_ui: Arc<Box<dyn UserInterface>>,
    ) -> Arc<Box<dyn UserInterface>> {
        Arc::new(Box::new(ProxyUI::new(
            real_ui,
            self.fragment_buffer.clone(),
            self.is_ui_connected.clone(),
            self.activity_state.clone(),
            self.session.id.clone(),
        )))
    }

    /// Generate UI events for connecting to this session
    /// Returns SetMessages event with all session messages including incomplete streaming message
    pub fn generate_session_connect_events(&self) -> Result<Vec<UiEvent>, anyhow::Error> {
        let mut events = Vec::new();

        // Convert session messages to UI data
        let mut messages_data = self.convert_messages_to_ui_data(self.session.tool_syntax)?;
        let tool_results = self.convert_tool_executions_to_ui_data()?;

        // If currently streaming, add incomplete message as additional MessageData
        let buffered_fragments = self.get_buffered_fragments(false); // Don't clear buffer
        if !buffered_fragments.is_empty() {
            // Create incomplete assistant message from buffered fragments
            let incomplete_message = MessageData {
                role: crate::ui::gpui::elements::MessageRole::Assistant,
                fragments: buffered_fragments,
            };
            messages_data.push(incomplete_message);
        }

        events.push(UiEvent::SetMessages {
            messages: messages_data,
            session_id: Some(self.session.id.clone()),
            tool_results,
        });

        events.push(UiEvent::UpdateMemory {
            memory: self.session.working_memory.clone(),
        });

        // Add current activity state for this session
        events.push(UiEvent::UpdateSessionActivityState {
            session_id: self.session.id.clone(),
            activity_state: self.get_activity_state(),
        });

        Ok(events)
    }

    /// Convert session messages to UI MessageData format
    pub fn convert_messages_to_ui_data(
        &self,
        tool_syntax: crate::types::ToolSyntax,
    ) -> Result<Vec<MessageData>, anyhow::Error> {
        // Create dummy UI for stream processor
        struct DummyUI;
        #[async_trait::async_trait]
        impl crate::ui::UserInterface for DummyUI {
            async fn send_event(
                &self,
                _event: crate::ui::UiEvent,
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
            fn should_streaming_continue(&self) -> bool {
                true
            }
            fn notify_rate_limit(&self, _seconds_remaining: u64) {
                // No-op for dummy UI
            }
            fn clear_rate_limit(&self) {
                // No-op for dummy UI
            }
        }

        let dummy_ui = std::sync::Arc::new(Box::new(DummyUI) as Box<dyn crate::ui::UserInterface>);
        let mut processor = create_stream_processor(tool_syntax, dummy_ui, 0);

        let mut messages_data = Vec::new();

        trace!(
            "preparing {} messages for event",
            self.session.messages.len()
        );

        for message in &self.session.messages {
            // Filter out tool-result user messages (they have tool IDs in structured content)
            if message.role == llm::MessageRole::User {
                match &message.content {
                    llm::MessageContent::Text(text) if text.trim().is_empty() => {
                        // Skip empty user messages (legacy tool results in XML mode)
                        continue;
                    }
                    llm::MessageContent::Structured(blocks) => {
                        // Check if this is a tool-result message by looking for ToolResult blocks
                        let has_tool_results = blocks
                            .iter()
                            .any(|block| matches!(block, llm::ContentBlock::ToolResult { .. }));

                        if has_tool_results {
                            // Skip tool-result user messages (they shouldn't be shown in UI)
                            continue;
                        }
                        // Otherwise, this is a real structured user message, process it
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
                    error!("Failed to extract fragments from message: {}", e);
                    // Continue with other messages even if one fails
                }
            }
        }

        trace!("prepared {} message data for event", messages_data.len());

        Ok(messages_data)
    }

    /// Convert tool executions to UI tool result data
    fn convert_tool_executions_to_ui_data(
        &self,
    ) -> Result<Vec<crate::ui::ui_events::ToolResultData>, anyhow::Error> {
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

            tool_results.push(crate::ui::ui_events::ToolResultData {
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
    is_session_connected: Arc<Mutex<bool>>,
    session_activity_state: Arc<Mutex<SessionActivityState>>,
    session_id: String,
}

impl ProxyUI {
    pub fn new(
        real_ui: Arc<Box<dyn UserInterface>>,
        fragment_buffer: Arc<Mutex<VecDeque<DisplayFragment>>>,
        is_session_connected: Arc<Mutex<bool>>,
        session_activity_state: Arc<Mutex<SessionActivityState>>,
        session_id: String,
    ) -> Self {
        Self {
            real_ui,
            fragment_buffer,
            is_session_connected,
            session_activity_state,
            session_id,
        }
    }

    /// Check if this session is currently connected to the real UI
    fn is_connected(&self) -> bool {
        self.is_session_connected
            .lock()
            .map(|active| *active)
            .unwrap_or(false)
    }

    /// Update activity state and broadcast the change to the UI
    fn update_activity_state(&self, new_state: SessionActivityState) {
        // Update our internal state
        if let Ok(mut state) = self.session_activity_state.lock() {
            if *state != new_state {
                *state = new_state.clone();

                // Always broadcast activity state changes to UI (regardless of connection status)
                // This ensures the chat sidebar shows current activity for all sessions
                let _ = tokio::runtime::Handle::try_current().map(|_| {
                    let ui = self.real_ui.clone();
                    let session_id = self.session_id.clone();
                    let activity_state = new_state;
                    tokio::spawn(async move {
                        let _ = ui
                            .send_event(UiEvent::UpdateSessionActivityState {
                                session_id,
                                activity_state,
                            })
                            .await;
                    });
                });
            }
        }
    }
}

#[async_trait]
impl UserInterface for ProxyUI {
    async fn send_event(&self, event: UiEvent) -> Result<(), UIError> {
        // Handle special events that need buffer management and activity state updates
        match &event {
            UiEvent::StreamingStarted(_) => {
                // Clear fragment buffer at start of new LLM request
                if let Ok(mut buffer) = self.fragment_buffer.lock() {
                    buffer.clear();
                }
                // Update activity state to waiting for response
                self.update_activity_state(SessionActivityState::WaitingForResponse);
            }
            UiEvent::StreamingStopped { .. } => {
                // Clear fragment buffer when LLM request ends - fragments are now part of message history
                if let Ok(mut buffer) = self.fragment_buffer.lock() {
                    buffer.clear();
                }
                // Update activity state back to agent running (it will be set to idle when agent completes)
                let current_state = self.session_activity_state.lock().unwrap().clone();
                if matches!(
                    current_state,
                    SessionActivityState::WaitingForResponse
                        | SessionActivityState::RateLimited { .. }
                ) {
                    self.update_activity_state(SessionActivityState::AgentRunning);
                }
            }
            _ => {}
        }

        if self.is_connected() {
            self.real_ui.send_event(event).await
        } else {
            Ok(()) // NOP if session not connected
        }
    }

    async fn get_input(&self) -> Result<String, UIError> {
        if self.is_connected() {
            self.real_ui.get_input().await
        } else {
            Ok(String::new()) // Return empty string if session not connected
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

        // First fragment indicates streaming has started - transition from WaitingForResponse
        let current_state = self.session_activity_state.lock().unwrap().clone();
        if matches!(current_state, SessionActivityState::WaitingForResponse) {
            self.update_activity_state(SessionActivityState::AgentRunning);
        }

        // Only forward to real UI if session is connected
        if self.is_connected() {
            self.real_ui.display_fragment(fragment)
        } else {
            Ok(())
        }
    }

    fn should_streaming_continue(&self) -> bool {
        if self.is_connected() {
            self.real_ui.should_streaming_continue()
        } else {
            true // Don't interrupt streaming if session is not connected
        }
    }

    fn notify_rate_limit(&self, seconds_remaining: u64) {
        // Update session activity state and broadcast
        self.update_activity_state(SessionActivityState::RateLimited { seconds_remaining });

        if self.is_connected() {
            self.real_ui.notify_rate_limit(seconds_remaining);
        }
        // No-op if session not connected
    }

    fn clear_rate_limit(&self) {
        // Update session activity state back to waiting for response
        self.update_activity_state(SessionActivityState::WaitingForResponse);

        if self.is_connected() {
            self.real_ui.clear_rate_limit();
        }
        // No-op if session not connected
    }
}
