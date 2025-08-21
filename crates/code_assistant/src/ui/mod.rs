pub mod backend;
pub mod gpui;
pub mod streaming;
pub mod terminal;
pub mod ui_events;
use async_trait::async_trait;
pub use streaming::DisplayFragment;
use thiserror::Error;
pub use ui_events::UiEvent;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ToolStatus {
    Pending, // Default status when a tool appears in the stream
    Running, // Tool is currently being executed
    Success, // Execution was successful
    Error,   // Error during execution
}

#[derive(Error, Debug)]
pub enum UIError {
    #[error("IO error: {0}")]
    IOError(#[from] std::io::Error),
}

#[async_trait]
pub trait UserInterface: Send + Sync {
    /// Send an event to the UI
    async fn send_event(&self, event: UiEvent) -> Result<(), UIError>;

    /// Display a streaming fragment with specific type information
    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError>;

    /// Check if streaming should continue
    fn should_streaming_continue(&self) -> bool;

    /// Notify the UI about rate limiting and countdown
    fn notify_rate_limit(&self, seconds_remaining: u64);

    /// Clear rate limit notification
    fn clear_rate_limit(&self);

    /// Downcast to Any for accessing concrete type methods
    #[allow(dead_code)]
    fn as_any(&self) -> &dyn std::any::Any;
}
