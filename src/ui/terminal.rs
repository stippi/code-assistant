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
    red: &'static str,
    magenta: &'static str,
    cyan: &'static str,
    gray: &'static str,
}

impl Colors {
    fn new() -> Self {
        Colors {
            reset: "\x1b[0m",
            dim: "\x1b[2m",
            italic: "\x1b[3m",
            bold: "\x1b[1m",
            blue: "\x1b[34m",
            green: "\x1b[32m",
            yellow: "\x1b[33m",
            red: "\x1b[31m",
            magenta: "\x1b[35m",
            cyan: "\x1b[36m",
            gray: "\x1b[90m",
        }
    }
}

// Tag types we need to process
enum TagType {
    None,
    ThinkingStart,
    ThinkingEnd,
    ToolStart,
    ToolEnd,
    ParamStart,
    ParamEnd,
}

// State for the streaming processor
struct FormattingState {
    // Buffer for collecting partial text
    buffer: String,
    // Track if we're inside thinking tags
    in_thinking: bool,
    // Track if we're inside tool tags
    in_tool: bool,
    // Current active tool name (if any)
    tool_name: String,
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
                buffer: String::new(),
                in_thinking: false,
                in_tool: false,
                tool_name: String::new(),
            })),
        }
    }

    async fn write_line(&self, s: &str) -> Result<(), UIError> {
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", s)?;
        Ok(())
    }

    fn format_tool_result(&self, text: &str) -> String {
        // Determine result type and choose appropriate symbol and color
        let (status_symbol, status_color) = if text.contains("Failed")
            || text.contains("Error")
            || text.contains("failed")
            || text.contains("error")
        {
            ("✗", self.colors.red)
        } else if text.contains("Successfully")
            || text.starts_with("Available")
            || text.contains("success")
        {
            ("✓", self.colors.green)
        } else {
            ("•", self.colors.blue)
        };

        // Format with clean header
        let formatted = format!(
            "\n{}{} {}Tool Result:{} ",
            status_color, status_symbol, self.colors.bold, self.colors.reset
        );

        // Apply highlighting to content
        let highlighted_text = text
            .replace(
                "- ",
                &format!("{}• {}", self.colors.blue, self.colors.reset),
            )
            .replace(
                "> ",
                &format!("{}▶ {}", self.colors.cyan, self.colors.reset),
            );

        format!("{}{}", formatted, highlighted_text)
    }

    // Detect what kind of tag we're seeing
    fn detect_tag(&self, text: &str) -> (TagType, usize) {
        if text.starts_with("<thinking>") {
            (TagType::ThinkingStart, 10)
        } else if text.starts_with("</thinking>") {
            (TagType::ThinkingEnd, 11)
        } else if text.starts_with("<tool:") {
            (TagType::ToolStart, 6)
        } else if text.starts_with("</tool:") {
            (TagType::ToolEnd, 7)
        } else if text.starts_with("<param:") {
            (TagType::ParamStart, 7)
        } else if text.starts_with("</param:") {
            (TagType::ParamEnd, 8)
        } else {
            (TagType::None, 0)
        }
    }

    // Find the closing > for a tag
    fn find_tag_end(&self, text: &str, start: usize) -> Option<usize> {
        text[start..].find('>').map(|pos| start + pos + 1)
    }

    // Check if a string is a potential beginning of a tag
    fn is_potential_tag_start(&self, text: &str) -> bool {
        // Tag prefixes to check for
        const TAG_PREFIXES: [&str; 6] = [
            "<thinking>",
            "</thinking>",
            "<tool:",
            "</tool:",
            "<param:",
            "</param:",
        ];

        // Check if the text could be the start of any tag
        for prefix in &TAG_PREFIXES {
            // Loop through all possible partial matches
            for i in 1..=prefix.len() {
                if i <= text.len() && &text[text.len() - i..] == &prefix[..i] {
                    return true;
                }
            }
        }

        false
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
                // Format questions with a clear indicator
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
        // Simple prompt character
        print!("{}> {}", self.colors.green, self.colors.reset);
        io::stdout().flush()?;

        let mut line = String::new();
        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        reader.read_line(&mut line).await?;

        Ok(line.trim().to_string())
    }

    fn display_streaming(&self, text: &str) -> Result<(), UIError> {
        let mut state = self.state.lock().unwrap();
        let mut stdout = io::stdout().lock();

        // Combine buffer with new text
        let current_text = format!("{}{}", state.buffer, text);

        // Clear buffer as we're processing it now
        state.buffer.clear();

        // Check if the end of text could be a partial tag
        // If so, save it to buffer and only process the rest
        let mut processing_text = current_text.clone();
        let mut safe_length = processing_text.len();

        // Check backwards for potential tag starts
        for j in (1..=processing_text.len().min(20)).rev() {
            // Check at most last 20 chars
            let suffix = &processing_text[processing_text.len() - j..];
            if self.is_potential_tag_start(suffix) {
                // We found a potential tag start, buffer this part
                safe_length = processing_text.len() - j;
                state.buffer = suffix.to_string();
                break;
            }
        }

        // Only process text up to safe_length
        if safe_length < processing_text.len() {
            processing_text = processing_text[..safe_length].to_string();
        } else {
            // No potential tag at end, clear buffer
            state.buffer.clear();
        }

        // Process text character by character for maximum control
        let mut i = 0;
        while i < processing_text.len() {
            // Look for tag indicators < and </
            if processing_text[i..].starts_with('<') {
                let (tag_type, tag_name_offset) = self.detect_tag(&processing_text[i..]);

                match tag_type {
                    TagType::ThinkingStart => {
                        // Output text before tag
                        if i > 0 {
                            write!(stdout, "{}", &processing_text[..i])?;
                        }

                        // Mark that we're in thinking mode
                        state.in_thinking = true;

                        // Skip past the opening tag
                        i += 10; // Length of "<thinking>"
                    }

                    TagType::ThinkingEnd => {
                        // Exit thinking mode
                        state.in_thinking = false;

                        // Skip past the closing tag
                        i += 11; // Length of "</thinking>"
                    }

                    TagType::ToolStart => {
                        // Look for the end of this tag
                        if let Some(end_pos) = self.find_tag_end(&processing_text, i) {
                            // Output text before tag if we're not in a thinking block
                            if !state.in_thinking && i > 0 {
                                write!(stdout, "{}", &processing_text[..i])?;
                            }

                            // Extract tool name
                            let tag_content = &processing_text[i..end_pos];
                            state.tool_name = if let Some(name_end) = tag_content[6..].find('>') {
                                tag_content[6..6 + name_end].to_string()
                            } else {
                                String::new()
                            };

                            // Output tool start
                            if !state.in_thinking {
                                write!(
                                    stdout,
                                    "\n{}⏺ {}{}{}",
                                    self.colors.cyan,
                                    self.colors.bold,
                                    state.tool_name,
                                    self.colors.reset
                                )?;
                            }

                            // Mark that we're inside a tool tag
                            state.in_tool = true;

                            // Skip past this tag
                            i = end_pos;
                        } else {
                            // Incomplete tag, put in buffer and exit
                            state.buffer = processing_text[i..].to_string();
                            break;
                        }
                    }

                    TagType::ToolEnd => {
                        // Look for the end of this tag
                        if let Some(end_pos) = self.find_tag_end(&processing_text, i) {
                            // Exit tool mode
                            state.in_tool = false;
                            state.tool_name = String::new();

                            // Skip past this tag
                            i = end_pos;
                        } else {
                            // Incomplete tag, put in buffer and exit
                            state.buffer = processing_text[i..].to_string();
                            break;
                        }
                    }

                    TagType::ParamStart => {
                        // Look for the end of this tag
                        if let Some(end_pos) = self.find_tag_end(&processing_text, i) {
                            // Format parameter start
                            if !state.in_thinking && state.in_tool {
                                write!(stdout, "\n  {} ┃{} ", self.colors.cyan, self.colors.reset)?;
                            }

                            // Skip past this tag
                            i = end_pos;
                        } else {
                            // Incomplete tag, put in buffer and exit
                            state.buffer = processing_text[i..].to_string();
                            break;
                        }
                    }

                    TagType::ParamEnd => {
                        // Look for the end of this tag
                        if let Some(end_pos) = self.find_tag_end(&processing_text, i) {
                            // Skip past this tag
                            i = end_pos;
                        } else {
                            // Incomplete tag, put in buffer and exit
                            state.buffer = processing_text[i..].to_string();
                            break;
                        }
                    }

                    TagType::None => {
                        // Not a tag, just output one character
                        if state.in_thinking {
                            // In thinking mode, apply styling
                            write!(
                                stdout,
                                "{}{}{}",
                                self.colors.dim,
                                self.colors.italic,
                                &processing_text[i..i + 1]
                            )?;
                        } else {
                            // Regular character, output directly
                            write!(stdout, "{}", &processing_text[i..i + 1])?;
                        }
                        i += 1;
                    }
                }
            } else {
                // Regular character (not part of a tag)
                if state.in_thinking {
                    // In thinking mode, apply styling
                    write!(
                        stdout,
                        "{}{}{}",
                        self.colors.dim,
                        self.colors.italic,
                        &processing_text[i..i + 1]
                    )?;
                } else {
                    // Regular character, output directly
                    write!(stdout, "{}", &processing_text[i..i + 1])?;
                }
                i += 1;
            }
        }

        // Reset style at end of output
        if state.in_thinking {
            write!(stdout, "{}", self.colors.reset)?;
        }

        stdout.flush()?;
        Ok(())
    }
}
