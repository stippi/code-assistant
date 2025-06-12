pub mod gpui;
pub mod streaming;
pub mod terminal;
use crate::types::WorkingMemory;
use async_trait::async_trait;
pub use streaming::DisplayFragment;
pub use gpui::ui_events::UiEvent;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ToolStatus {
    Pending, // Default status when a tool appears in the stream
    Running, // Tool is currently being executed
    Success, // Execution was successful
    Error,   // Error during execution
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StreamingState {
    Idle,          // No active streaming, ready to send
    Streaming,     // Currently streaming response
    StopRequested, // User requested stop, waiting for stream to end
}

#[derive(Debug, Clone)]
pub enum UIMessage {
    // System actions that the agent takes
    Action(String),
    // User input messages
    UserInput(String),
    // UI events for GPUI interface
    UiEvent(UiEvent),
}

#[derive(Error, Debug)]
pub enum UIError {
    #[error("IO error: {0}")]
    IOError(#[from] std::io::Error),
    #[error("Input not supported in this UI mode")]
    InputNotSupported,
    // #[error("Input cancelled")]
    // Cancelled,
    // #[error("Other UI error: {0}")]
    // Other(String),
}

#[async_trait]
pub trait UserInterface: Send + Sync {
    /// Display a message to the user
    async fn display(&self, message: UIMessage) -> Result<(), UIError>;

    /// Get input from the user
    async fn get_input(&self) -> Result<String, UIError>;

    /// Display a streaming fragment with specific type information
    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError>;

    /// Update tool status for a specific tool
    async fn update_tool_status(
        &self,
        tool_id: &str,
        status: ToolStatus,
        message: Option<String>,
        output: Option<String>,
    ) -> Result<(), UIError>;

    /// Update memory view with current working memory
    async fn update_memory(&self, memory: &WorkingMemory) -> Result<(), UIError>;

    /// Informs the UI that a new LLM request is starting
    /// Returns the request ID that can be used to correlate tool invocations
    async fn begin_llm_request(&self) -> Result<u64, UIError>;

    /// Informs the UI that an LLM request has completed
    async fn end_llm_request(&self, request_id: u64, cancelled: bool) -> Result<(), UIError>;

    /// Check if streaming should continue
    fn should_streaming_continue(&self) -> bool;
}

#[cfg(test)]
mod terminal_test;
