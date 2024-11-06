pub mod terminal;
use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug)]
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
}
