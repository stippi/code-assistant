use anyhow::Result;
use async_trait::async_trait;
use tokio::io::{AsyncWriteExt, Stdout};

#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use tokio::sync::Mutex as TokioMutex;

/// A trait for writing messages to an output stream.
/// This abstraction allows replacing the actual output writer in tests.
#[async_trait]
pub trait MessageWriter: Send + Sync {
    /// Write a message to the output stream and flush it.
    async fn write_message(&mut self, message: &str) -> Result<()>;
}

/// The default implementation of MessageWriter that writes to Stdout.
pub struct StdoutWriter {
    stdout: Stdout,
}

impl StdoutWriter {
    pub fn new(stdout: Stdout) -> Self {
        Self { stdout }
    }
}

#[async_trait]
impl MessageWriter for StdoutWriter {
    async fn write_message(&mut self, message: &str) -> Result<()> {
        self.stdout.write_all(message.as_bytes()).await?;
        self.stdout.write_all(b"\n").await?;
        self.stdout.flush().await?;
        Ok(())
    }
}

/// A mock writer implementation for testing.
#[cfg(test)]
pub struct MockWriter {
    /// Stores all messages written to this writer
    pub messages: Arc<TokioMutex<Vec<String>>>,
}

#[cfg(test)]
impl MockWriter {
    pub fn new() -> Self {
        Self {
            messages: Arc::new(TokioMutex::new(Vec::new())),
        }
    }

    /// Get a clone of all messages that have been written
    pub async fn get_messages(&self) -> Vec<String> {
        self.messages.lock().await.clone()
    }
}

#[cfg(test)]
#[async_trait]
impl MessageWriter for MockWriter {
    async fn write_message(&mut self, message: &str) -> Result<()> {
        let mut messages = self.messages.lock().await;
        messages.push(message.to_string());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_stdout_writer() {
        // This is a simple test that just verifies the writer doesn't error
        // We can't easily test the actual output to stdout in a unit test
        let mut writer = StdoutWriter::new(tokio::io::stdout());
        let result = writer.write_message("Test message").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_mock_writer() {
        let mut writer = MockWriter::new();

        // Write some messages
        writer.write_message("Message 1").await.unwrap();
        writer.write_message("Message 2").await.unwrap();
        writer.write_message("Message 3").await.unwrap();

        // Verify the messages were stored
        let messages = writer.get_messages().await;
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0], "Message 1");
        assert_eq!(messages[1], "Message 2");
        assert_eq!(messages[2], "Message 3");

        // Clone the messages Arc for testing in another scope
        let messages_arc = writer.messages.clone();

        // Ensure the message can be accessed from multiple places
        {
            let mut messages = messages_arc.lock().await;
            messages.push("Message 4".to_string());
        }

        let messages = writer.get_messages().await;
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[3], "Message 4");
    }
}
