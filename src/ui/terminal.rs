use super::{UIError, UIMessage, UserInterface};
use async_trait::async_trait;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};

// ANSI color codes for terminal formatting
struct Colors {
    reset: &'static str,
    dim: &'static str,
    bold: &'static str,
    italic: &'static str,
    blue: &'static str,
    green: &'static str,
    yellow: &'static str,
    magenta: &'static str,
    cyan: &'static str,
    gray: &'static str,
}

impl Colors {
    fn new() -> Self {
        Colors {
            reset: "\x1b[0m",
            dim: "\x1b[2m",
            bold: "\x1b[1m",
            italic: "\x1b[3m",
            blue: "\x1b[34m",
            green: "\x1b[32m",
            yellow: "\x1b[33m",
            magenta: "\x1b[35m",
            cyan: "\x1b[36m",
            gray: "\x1b[90m",
        }
    }
}

// For formatting thinking process output
struct FormattingState {
    is_in_thinking: bool,
    thinking_buffer: String,
}

pub struct TerminalUI {
    colors: Colors,
    state: Arc<Mutex<FormattingState>>,
}

impl TerminalUI {
    pub fn new() -> Self {
        Self {
            colors: Colors::new(),
            state: Arc::new(Mutex::new(FormattingState {
                is_in_thinking: false,
                thinking_buffer: String::new(),
            })),
        }
    }

    async fn write_line(&self, s: &str) -> Result<(), UIError> {
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", s)?;
        Ok(())
    }

    fn format_tool_result(&self, text: &str) -> String {
        let mut formatted = String::new();

        // Add a visual separator for tool results
        formatted.push_str(&format!(
            "\n{}┌─── TOOL RESULT ───┐{}\n",
            self.colors.gray, self.colors.reset
        ));

        // Bold and colorize tool results for better visibility
        if text.starts_with("Successfully") {
            formatted.push_str(&format!(
                "{}{}✓ {}{}",
                self.colors.green, self.colors.bold, text, self.colors.reset
            ));
        } else if text.starts_with("Failed") || text.starts_with("Error") {
            formatted.push_str(&format!(
                "{}{}✗ {}{}",
                self.colors.yellow, self.colors.bold, text, self.colors.reset
            ));
        } else {
            // Highlight specific result patterns in regular output
            let highlighted = text
                .replace(
                    "- ",
                    &format!("{}• {}", self.colors.blue, self.colors.reset),
                )
                .replace(
                    "> ",
                    &format!(
                        "{}{}>>{} ",
                        self.colors.magenta, self.colors.bold, self.colors.reset
                    ),
                );
            formatted.push_str(&highlighted);
        }

        // Add closing separator
        formatted.push_str(&format!(
            "\n{}└─────────────────────┘{}",
            self.colors.gray, self.colors.reset
        ));

        formatted
    }

    // Process tool calls with a more careful approach
    fn format_tool_calls(&self, text: &str) -> String {
        // If no tool tags, return the original text
        if !text.contains("<tool:") && !text.contains("<param:") {
            return text.to_string();
        }

        // We need to be careful with replacements to preserve the actual XML
        // Let's just do some minimal highlighting that enhances readability without breaking the XML

        let formatted = text
            // Add a visual box around tool invocations
            .replace(
                "<tool:",
                &format!(
                    "\n\n{}{}┏━━ TOOL INVOCATION ━━━━━━━━━━┓{}\n<tool:",
                    self.colors.cyan, self.colors.bold, self.colors.reset
                ),
            )
            // Format parameters with clear indentation
            .replace(
                "<param:",
                &format!(
                    "\n   {}{}┃{} <param:",
                    self.colors.cyan, self.colors.bold, self.colors.reset
                ),
            )
            // Format parameter values nicely
            .replace(
                "</param:",
                &format!(
                    "\n   {}{}┃{} </param:",
                    self.colors.cyan, self.colors.bold, self.colors.reset
                ),
            )
            // Close the visual box
            .replace(
                "</tool:",
                &format!(
                    "\n{}{}┗━━━━━━━━━━━━━━━━━━━━━━━━━━━┛{}\n</tool:",
                    self.colors.cyan, self.colors.bold, self.colors.reset
                ),
            );

        formatted
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
                // Format questions - use a nice format but without the prompt character
                // The prompt will be added later by get_input()
                let formatted_question = format!(
                    "\n{}{}Question:{} {}",
                    self.colors.cyan, self.colors.bold, self.colors.reset, msg
                );
                self.write_line(&formatted_question).await?
            }
        }
        Ok(())
    }

    async fn get_input(&self, _prompt: &str) -> Result<String, UIError> {
        // The simplest possible approach - a basic prompt character
        print!("{}> {}", self.colors.green, self.colors.reset);
        io::stdout().flush()?;

        // Use standard line reading for maximum compatibility with terminal input handling
        let mut line = String::new();
        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        reader.read_line(&mut line).await?;

        Ok(line.trim().to_string())
    }

    fn display_streaming(&self, text: &str) -> Result<(), UIError> {
        let mut state_guard = self.state.lock().unwrap();
        let mut formatted_text = String::new();
        let mut stdout = io::stdout().lock();

        // Process the text for thinking sections
        if state_guard.is_in_thinking {
            if text.contains("</thinking>") {
                // End of thinking section
                let parts: Vec<&str> = text.split("</thinking>").collect();
                state_guard.thinking_buffer.push_str(parts[0]);

                // Get a copy of the buffer to use after we clear it
                let buffer_content = state_guard.thinking_buffer.clone();

                // Format the entire thinking section
                formatted_text.push_str(&format!(
                    "{}{}{}{}",
                    self.colors.dim, self.colors.italic, buffer_content, self.colors.reset
                ));

                // Reset buffer and state
                state_guard.thinking_buffer.clear();
                state_guard.is_in_thinking = false;

                // Add any text after </thinking>
                if parts.len() > 1 && !parts[1].is_empty() {
                    formatted_text.push_str(&self.format_tool_calls(parts[1]));
                }
            } else {
                // Still inside thinking tag, keep buffering
                state_guard.thinking_buffer.push_str(text);
                // Don't output anything yet
                return Ok(());
            }
        } else if text.contains("<thinking>") {
            // Start of thinking section
            let parts: Vec<&str> = text.split("<thinking>").collect();

            // Output any text before <thinking>
            if !parts[0].is_empty() {
                formatted_text.push_str(&self.format_tool_calls(parts[0]));
            }

            // Start buffering thinking content
            state_guard.is_in_thinking = true;
            state_guard.thinking_buffer.clear();

            if parts.len() > 1 {
                state_guard.thinking_buffer.push_str(parts[1]);

                // Check if thinking block ends in this chunk
                if state_guard.thinking_buffer.contains("</thinking>") {
                    // Get the parts and process them
                    let buffer_content = state_guard.thinking_buffer.clone();
                    let inner_parts: Vec<&str> = buffer_content.split("</thinking>").collect();

                    // Format the thinking content
                    formatted_text.push_str(&format!(
                        "{}{}{}{}",
                        self.colors.dim, self.colors.italic, inner_parts[0], self.colors.reset
                    ));

                    // Reset state
                    state_guard.thinking_buffer.clear();
                    state_guard.is_in_thinking = false;

                    // Add any text after </thinking>
                    if inner_parts.len() > 1 && !inner_parts[1].is_empty() {
                        formatted_text.push_str(&self.format_tool_calls(inner_parts[1]));
                    }
                } else {
                    // Thinking continues in next chunk
                    return Ok(());
                }
            }
        } else {
            // Regular text, format tool calls if any
            formatted_text = self.format_tool_calls(text);
        }

        // Output the formatted text
        write!(stdout, "{}", formatted_text)?;
        stdout.flush()?;
        Ok(())
    }
}
