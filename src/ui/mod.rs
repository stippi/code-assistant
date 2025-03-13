pub mod gpui;
pub mod streaming;
pub mod terminal;
use async_trait::async_trait;
pub use streaming::DisplayFragment;
use thiserror::Error;

#[derive(Debug, Clone)]
pub enum UIMessage {
    // System actions that the agent takes
    Action(String),
    // Questions to the user that need a response
    Question(String),
}

#[derive(Error, Debug)]
pub enum UIError {
    #[error("IO error: {0}")]
    IOError(#[from] std::io::Error),
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
    async fn get_input(&self, prompt: &str) -> Result<String, UIError>;

    /// Display streaming output synchronously (legacy method, still needed for compatibility)
    fn display_streaming(&self, text: &str) -> Result<(), UIError>;

    /// Display a streaming fragment with specific type information
    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError> {
        // Default implementation converts fragments to text for backward compatibility
        match fragment {
            DisplayFragment::PlainText(text) => self.display_streaming(text),
            DisplayFragment::ThinkingText(text) => self.display_streaming(text),
            DisplayFragment::ToolName { name, .. } => {
                self.display_streaming(&format!("\n• {}", name))
            }
            DisplayFragment::ToolParameter { name, value, .. } => {
                self.display_streaming(&format!("  {}: {}", name, value))
            }
            DisplayFragment::ToolEnd { .. } => Ok(()),
        }
    }
}

#[cfg(test)]
mod terminal_test;

#[cfg(test)]
mod streaming_test;
