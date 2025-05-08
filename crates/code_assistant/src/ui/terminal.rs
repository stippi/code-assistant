use super::{DisplayFragment, ToolStatus, UIError, UIMessage, UserInterface};
use crate::types::WorkingMemory;
use async_trait::async_trait;
use crossterm::{
    style::{self, Color, Stylize},
    terminal::{self},
};
use rustyline::{error::ReadlineError, history::DefaultHistory, Config, Editor};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

pub struct TerminalUI {
    // For line editor - using just the default implementation without custom helper
    line_editor: Arc<Mutex<Editor<(), DefaultHistory>>>,
    // In production code, this isn't used
    writer: Option<Arc<Mutex<Box<dyn Write + Send>>>>,
}

impl TerminalUI {
    pub fn new() -> Self {
        // Initialize rustyline with configuration
        let config = Config::builder()
            .edit_mode(rustyline::EditMode::Emacs)
            .build();

        // Create editor with default helper
        let editor = Editor::with_config(config).expect("Failed to create line editor");

        Self {
            line_editor: Arc::new(Mutex::new(editor)),
            writer: None,
        }
    }

    #[cfg(test)]
    pub fn with_test_writer(writer: Box<dyn Write + Send>) -> Self {
        // Similar to new() but with test writer
        let config = Config::builder()
            .edit_mode(rustyline::EditMode::Emacs)
            .build();

        // Create editor with default helper
        let editor = Editor::with_config(config).expect("Failed to create line editor");

        Self {
            line_editor: Arc::new(Mutex::new(editor)),
            writer: Some(Arc::new(Mutex::new(writer))),
        }
    }

    async fn write_line(&self, s: &str) -> Result<(), UIError> {
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", s)?;
        Ok(())
    }

    // Create a frame around content
    fn frame_content(&self, content: &str, title: Option<&str>, color: Color) -> String {
        // Get terminal width
        let (width, _) = terminal::size().unwrap_or((80, 24));
        let frame_width = (width as usize).min(100); // Cap at 100 columns

        // Use the specified color
        let border_color = color;

        // Split content into lines
        let lines: Vec<&str> = content.lines().collect();

        // Build the frame
        let mut result = String::new();

        // Top border with optional title
        result.push_str(&format!("{}╭", "─".repeat(2).with(border_color)));

        if let Some(t) = title {
            result.push_str(&format!(
                "{}─{}",
                "─".to_string().with(border_color),
                format!(" {} ", t).bold().with(border_color)
            ));
            let remaining = frame_width.saturating_sub(t.len() + 6);
            result.push_str(&format!("{}", "─".repeat(remaining).with(border_color)));
        } else {
            result.push_str(&format!(
                "{}",
                "─".repeat(frame_width - 3).with(border_color)
            ));
        }

        result.push_str(&format!("{}╮\n", "─".to_string().with(border_color)));

        // Content lines
        for line in lines {
            result.push_str(&format!(
                "{} {} {}\n",
                "│".with(border_color),
                line,
                "│".with(border_color)
            ));
        }

        // Bottom border
        result.push_str(&format!(
            "{}╰{}╯\n",
            "─".repeat(2).with(border_color),
            "─".repeat(frame_width - 3).with(border_color)
        ));

        result
    }

    fn format_tool_result(&self, text: &str) -> String {
        // Determine result type and choose appropriate color and symbol
        let (status_symbol, status_color) = if text.contains("Failed")
            || text.contains("Error")
            || text.contains("failed")
            || text.contains("error")
        {
            ("✗", Color::Red)
        } else if text.contains("Successfully")
            || text.starts_with("Available")
            || text.contains("success")
        {
            ("✓", Color::Green)
        } else {
            ("•", Color::Blue)
        };

        // Apply highlighting to content
        let highlighted_text = text
            .replace("- ", &format!("{} ", "•".with(Color::Blue)))
            .replace("> ", &format!("{} ", "▶".with(Color::Cyan)));

        // Combine status symbol and content
        format!("{} {}", status_symbol.with(status_color), highlighted_text)
    }
}

#[async_trait]
impl UserInterface for TerminalUI {
    async fn display(&self, message: UIMessage) -> Result<(), UIError> {
        match message {
            UIMessage::Action(msg) => {
                // Format tool results
                let formatted_msg = self.format_tool_result(&msg);
                self.write_line(&formatted_msg).await?
            }
            UIMessage::Question(msg) => {
                // Format questions with a frame
                let formatted_question = self.frame_content(&msg, Some("Question"), Color::Cyan);
                self.write_line(&formatted_question).await?
            }
            _ => {}
        }
        Ok(())
    }

    async fn get_input(&self, prompt: &str) -> Result<String, UIError> {
        // Access the editor
        let mut editor = self.line_editor.lock().unwrap();

        // Set a prompt with color
        let colored_prompt = format!(
            "{}{} ",
            if prompt.is_empty() {
                ">".with(Color::Green)
            } else {
                prompt.with(Color::Green)
            },
            style::ResetColor
        );

        // Read a line
        match editor.readline(&colored_prompt) {
            Ok(line) => {
                // Add to history
                let _ = editor.add_history_entry(line.as_str());
                Ok(line.trim().to_string())
            }
            Err(ReadlineError::Interrupted) => {
                // Ctrl-C
                Err(UIError::IOError(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Input interrupted",
                )))
            }
            Err(ReadlineError::Eof) => {
                // Ctrl-D
                Err(UIError::IOError(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Input EOF",
                )))
            }
            Err(e) => Err(UIError::IOError(io::Error::new(
                io::ErrorKind::Other,
                format!("Input error: {}", e),
            ))),
        }
    }

    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError> {
        // Get the appropriate writer (stdout or test writer)
        let mut stdout = io::stdout().lock();
        let writer: &mut dyn Write = if let Some(w) = &self.writer {
            // We have a test writer
            &mut *w.lock().unwrap()
        } else {
            // Use stdout in production
            &mut stdout
        };

        match fragment {
            DisplayFragment::PlainText(text) => {
                // Normal text, output as-is
                write!(writer, "{}", text)?;
            }
            DisplayFragment::ThinkingText(text) => {
                // Format thinking text with crossterm
                let styled_text = text.clone().dark_grey().italic();
                write!(writer, "{}", styled_text)?;
            }
            DisplayFragment::ToolName { name, .. } => {
                // Format tool name in bold blue with bullet point
                write!(writer, "\n• {}", name.to_string().bold().blue())?;
            }
            DisplayFragment::ToolParameter { name, value, .. } => {
                // Format parameter name in cyan with indentation
                write!(writer, "  {}: ", name.clone().cyan())?;
                // Parameter value in normal text
                write!(writer, "{}", value)?;
            }
            DisplayFragment::ToolEnd { .. } => {
                // No special formatting needed at tool end
            }
        }

        writer.flush()?;
        Ok(())
    }

    async fn update_tool_status(
        &self,
        _tool_id: &str,
        status: ToolStatus,
        message: Option<String>,
    ) -> Result<(), UIError> {
        // For terminal UI, we just print a status message if provided
        if let Some(msg) = message {
            // Choose color based on status
            let color = match status {
                ToolStatus::Pending => Color::DarkGrey,
                ToolStatus::Running => Color::Blue,
                ToolStatus::Success => Color::Green,
                ToolStatus::Error => Color::Red,
            };

            // Format status symbol
            let symbol = match status {
                ToolStatus::Pending => "⋯",
                ToolStatus::Running => "⚙",
                ToolStatus::Success => "✓",
                ToolStatus::Error => "✗",
            };

            // Format and print message
            let formatted_msg = format!("{} {}", symbol.with(color), msg);
            self.write_line(&formatted_msg).await?;
        }

        Ok(())
    }

    async fn update_memory(&self, _memory: &WorkingMemory) -> Result<(), UIError> {
        // Terminal UI doesn't display memory visually, so this is a no-op
        Ok(())
    }

    async fn begin_llm_request(&self) -> Result<u64, UIError> {
        // Use a simple timestamp for request IDs
        let request_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Optionally display a message that we're starting a new request
        self.write_line(
            &format!("Starting new LLM request ({})", request_id)
                .dark_blue()
                .to_string(),
        )
        .await?;

        Ok(request_id)
    }

    async fn end_llm_request(&self, request_id: u64) -> Result<(), UIError> {
        // Optionally display a message that the request has completed
        self.write_line(
            &format!("Completed LLM request ({})", request_id)
                .dark_blue()
                .to_string(),
        )
        .await?;

        Ok(())
    }
}
