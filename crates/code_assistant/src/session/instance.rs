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
use tracing::{debug, error, trace};

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
}

impl SessionInstance {
    /// Create a new session instance
    pub fn new(session: ChatSession) -> Self {
        Self {
            session,
            task_handle: None,
            fragment_buffer: Arc::new(Mutex::new(VecDeque::new())),
            is_ui_connected: Arc::new(Mutex::new(false)),
        }
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

    /// Reload session data from persistence
    /// This ensures SessionInstance has the latest state even if agents have made changes
    pub fn reload_from_persistence(
        &mut self,
        persistence: &crate::persistence::FileStatePersistence,
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
        )))
    }

    /// Generate UI events for connecting to this session
    /// Returns SetMessages event with all session messages including incomplete streaming message
    pub fn generate_session_connect_events(&self) -> Result<Vec<UiEvent>, anyhow::Error> {
        let mut events = Vec::new();

        // Convert session messages to UI data
        let mut messages_data = self.convert_messages_to_ui_data(self.session.tool_mode)?;
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
            async fn begin_llm_request(&self, request_id: u64) -> Result<(), crate::ui::UIError> {
                let _ = request_id;
                Ok(())
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
        let mut processor = create_stream_processor(tool_mode, dummy_ui, 0);

        let mut messages_data = Vec::new();

        trace!(
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
    is_session_connected: Arc<Mutex<bool>>,
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
            is_session_connected: is_session_active,
        }
    }

    /// Check if this session is currently connected to the real UI
    fn is_connected(&self) -> bool {
        self.is_session_connected
            .lock()
            .map(|active| *active)
            .unwrap_or(false)
    }
}

#[async_trait]
impl UserInterface for ProxyUI {
    async fn display(&self, message: UIMessage) -> Result<(), UIError> {
        if self.is_connected() {
            self.real_ui.display(message).await
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

        // Only forward to real UI if session is connected
        if self.is_connected() {
            self.real_ui.display_fragment(fragment)
        } else {
            Ok(())
        }
    }

    async fn update_memory(&self, memory: &crate::types::WorkingMemory) -> Result<(), UIError> {
        if self.is_connected() {
            self.real_ui.update_memory(memory).await
        } else {
            Ok(()) // NOP if session not connected
        }
    }

    async fn update_tool_status(
        &self,
        tool_id: &str,
        status: crate::ui::ToolStatus,
        message: Option<String>,
        output: Option<String>,
    ) -> Result<(), UIError> {
        if self.is_connected() {
            self.real_ui
                .update_tool_status(tool_id, status, message, output)
                .await
        } else {
            Ok(()) // NOP if session not connected
        }
    }

    async fn begin_llm_request(&self, request_id: u64) -> Result<(), UIError> {
        // Clear fragment buffer at start of new LLM request
        if let Ok(mut buffer) = self.fragment_buffer.lock() {
            buffer.clear();
        }

        if self.is_connected() {
            self.real_ui.begin_llm_request(request_id).await
        } else {
            Ok(()) // No-op if session not connected
        }
    }

    async fn end_llm_request(&self, request_id: u64, cancelled: bool) -> Result<(), UIError> {
        // Clear fragment buffer when LLM request ends - fragments are now part of message history
        if let Ok(mut buffer) = self.fragment_buffer.lock() {
            buffer.clear();
        }

        if self.is_connected() {
            self.real_ui.end_llm_request(request_id, cancelled).await
        } else {
            Ok(()) // NOP if session not connected
        }
    }

    fn should_streaming_continue(&self) -> bool {
        if self.is_connected() {
            self.real_ui.should_streaming_continue()
        } else {
            true // Don't interrupt streaming if session is not connected
        }
    }
}
