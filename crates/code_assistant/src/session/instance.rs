use anyhow::Result;
use llm::Message;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

// Agent instances are created on-demand, no need to import
use crate::ui::DisplayFragment;
use crate::persistence::ChatSession;

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
            streaming_message_id: None,
            agent_completed: false,
            last_agent_error: None,
        }
    }

    /// Check if the agent is currently running
    pub fn is_agent_running(&self) -> bool {
        self.task_handle.is_some() && !self.agent_completed
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

    /// Add a display fragment to the buffer
    pub fn add_fragment(&self, fragment: DisplayFragment) {
        if let Ok(mut buffer) = self.fragment_buffer.lock() {
            buffer.push_back(fragment);

            // Keep buffer size reasonable (e.g., last 1000 fragments)
            while buffer.len() > 1000 {
                buffer.pop_front();
            }
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
    pub async fn terminate_agent(&mut self) {
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

    /// Get the last message ID for streaming identification
    pub fn get_last_message_id(&self) -> String {
        format!("msg_{}_{}", self.session.id, self.session.messages.len())
    }
}
