use super::{UIError, UIMessage, UserInterface};
use async_trait::async_trait;
use std::io::{self, Write};
use tokio::io::{AsyncBufReadExt, BufReader};

pub struct TerminalUI; // Simplified struct, no fields needed

impl TerminalUI {
    pub fn new() -> Self {
        Self
    }

    async fn write_line(&self, s: &str) -> Result<(), UIError> {
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", s)?;
        Ok(())
    }
}

#[async_trait]
impl UserInterface for TerminalUI {
    async fn display(&self, message: UIMessage) -> Result<(), UIError> {
        match message {
            UIMessage::Action(msg) => self.write_line(&msg).await?,
            UIMessage::Question(msg) => self.write_line(&format!("{}\n> ", msg)).await?,
            UIMessage::Result(msg) => self.write_line(&format!("Result: {}", msg)).await?,
            UIMessage::Debug(msg) => {
                if std::env::var("DEBUG").is_ok() {
                    self.write_line(&format!("Debug: {}", msg)).await?
                }
            }
        }
        Ok(())
    }

    async fn get_input(&self, prompt: &str) -> Result<String, UIError> {
        print!("{}", prompt);
        io::stdout().flush()?;

        let mut line = String::new();
        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        reader.read_line(&mut line).await?;

        Ok(line.trim().to_string())
    }
}
