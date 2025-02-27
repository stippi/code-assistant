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
    // yellow: &'static str,
    red: &'static str,
    // magenta: &'static str,
    cyan: &'static str,
    // gray: &'static str,
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
            // yellow: "\x1b[33m",
            red: "\x1b[31m",
            // magenta: "\x1b[35m",
            cyan: "\x1b[36m",
            // gray: "\x1b[90m",
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
    // Track if we're inside param tags
    in_param: bool,
    // Current active tool name (if any)
    tool_name: String,
}

pub struct TerminalUI {
    colors: Colors,
    state: Arc<Mutex<FormattingState>>,
    // In production code, this isn't used
    writer: Option<Arc<Mutex<Box<dyn Write + Send>>>>,
}

impl TerminalUI {
    pub fn new() -> Self {
        Self {
            colors: Colors::new(),
            state: Arc::new(Mutex::new(FormattingState {
                buffer: String::new(),
                in_thinking: false,
                in_tool: false,
                in_param: false,
                tool_name: String::new(),
            })),
            writer: None,
        }
    }

    #[cfg(test)]
    pub fn with_test_writer(writer: Box<dyn Write + Send>) -> Self {
        Self {
            colors: Colors::new(),
            state: Arc::new(Mutex::new(FormattingState {
                buffer: String::new(),
                in_thinking: false,
                in_tool: false,
                in_param: false,
                tool_name: String::new(),
            })),
            writer: Some(Arc::new(Mutex::new(writer))),
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

        // Get the appropriate writer (stdout or test writer)
        let mut stdout = io::stdout().lock();
        let writer: &mut dyn Write = if let Some(w) = &self.writer {
            // We have a test writer
            &mut *w.lock().unwrap()
        } else {
            // Use stdout in production
            &mut stdout
        };

        // Combine buffer with new text
        let current_text = format!("{}{}", state.buffer, text);

        // Check if the end of text could be a partial tag
        // If so, save it to buffer and only process the rest
        let mut processing_text = current_text.clone();
        let mut safe_length = processing_text.len();

        // Check backwards for potential tag starts
        for j in (1..=processing_text.len().min(40)).rev() {
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

        // Process the buffered text as chunks
        let mut current_pos = 0;

        // While we have content to process
        while current_pos < processing_text.len() {
            // Look for next tag
            if let Some(tag_pos) = processing_text[current_pos..].find('<') {
                let absolute_tag_pos = current_pos + tag_pos;

                // Output all text before this tag if there is any
                if tag_pos > 0 {
                    let pre_tag_text = &processing_text[current_pos..absolute_tag_pos];
                    if state.in_thinking {
                        // Format thinking text
                        write!(
                            writer,
                            "{}{}{}",
                            self.colors.dim, self.colors.italic, pre_tag_text
                        )?;
                    } else {
                        // Normal text, output as-is
                        write!(writer, "{}", pre_tag_text)?;
                    }
                }

                // Determine what kind of tag we're looking at
                let tag_slice = &processing_text[absolute_tag_pos..];
                let (tag_type, _) = self.detect_tag(tag_slice);

                match tag_type {
                    TagType::ThinkingStart => {
                        // Mark that we're in thinking mode
                        state.in_thinking = true;

                        // Skip past this tag
                        if absolute_tag_pos + 10 <= processing_text.len() {
                            current_pos = absolute_tag_pos + 10;
                        } else {
                            // Incomplete tag, buffer the rest
                            state.buffer = processing_text[absolute_tag_pos..].to_string();
                            break;
                        }
                    }

                    TagType::ThinkingEnd => {
                        // Exit thinking mode and reset formatting
                        state.in_thinking = false;
                        write!(writer, "{}", self.colors.reset)?;

                        // Skip past this tag
                        if absolute_tag_pos + 11 <= processing_text.len() {
                            current_pos = absolute_tag_pos + 11;
                        } else {
                            // Incomplete tag, buffer the rest
                            state.buffer = processing_text[absolute_tag_pos..].to_string();
                            break;
                        }
                    }

                    TagType::ToolStart => {
                        // See if we can find the end of this opening tag
                        if let Some(end_pos) = tag_slice.find('>') {
                            let tool_name = if end_pos > 6 {
                                &tag_slice[6..end_pos]
                            } else {
                                "unknown"
                            };

                            // Output tool start
                            if !state.in_thinking {
                                write!(
                                    writer,
                                    "\n{}⏺ {}{}{}",
                                    self.colors.cyan,
                                    self.colors.bold,
                                    tool_name,
                                    self.colors.reset
                                )?;
                            }

                            // Mark that we're inside a tool tag
                            state.in_tool = true;
                            state.tool_name = tool_name.to_string();

                            // Skip past this tag
                            current_pos = absolute_tag_pos + end_pos + 1;
                        } else {
                            // Incomplete tag, buffer the rest
                            state.buffer = processing_text[absolute_tag_pos..].to_string();
                            break;
                        }
                    }

                    TagType::ToolEnd => {
                        // Look for the end of this closing tag
                        if let Some(end_pos) = tag_slice.find('>') {
                            // Exit tool mode
                            state.in_tool = false;
                            state.tool_name = String::new();

                            // Skip past this tag
                            current_pos = absolute_tag_pos + end_pos + 1;
                        } else {
                            // Incomplete tag, buffer the rest
                            state.buffer = processing_text[absolute_tag_pos..].to_string();
                            break;
                        }
                    }

                    TagType::ParamStart => {
                        // Look for the end of this parameter start tag
                        if let Some(end_pos) = tag_slice.find('>') {
                            // Format parameter start
                            if !state.in_thinking && state.in_tool {
                                write!(writer, "\n  {} ┃{} ", self.colors.cyan, self.colors.reset)?;
                            }

                            // Mark that we're in a parameter
                            state.in_param = true;

                            // Skip past this tag
                            current_pos = absolute_tag_pos + end_pos + 1;
                        } else {
                            // Incomplete tag, buffer the rest
                            state.buffer = processing_text[absolute_tag_pos..].to_string();
                            break;
                        }
                    }

                    TagType::ParamEnd => {
                        // Look for the end of this parameter end tag
                        if let Some(end_pos) = tag_slice.find('>') {
                            // We exit param mode but don't render the end tag
                            state.in_param = false;

                            // Skip past this tag without rendering it
                            current_pos = absolute_tag_pos + end_pos + 1;
                        } else {
                            // Incomplete tag, buffer the rest
                            state.buffer = processing_text[absolute_tag_pos..].to_string();
                            break;
                        }
                    }

                    TagType::None => {
                        // Not a recognized tag, treat as regular character
                        if state.in_thinking {
                            write!(
                                writer,
                                "{}{}{}",
                                self.colors.dim,
                                self.colors.italic,
                                &processing_text[absolute_tag_pos..absolute_tag_pos + 1]
                            )?;
                        } else {
                            write!(
                                writer,
                                "{}",
                                &processing_text[absolute_tag_pos..absolute_tag_pos + 1]
                            )?;
                        }
                        current_pos = absolute_tag_pos + 1;
                    }
                }
            } else {
                // No more tags, output the rest of the text
                let remaining = &processing_text[current_pos..];
                if state.in_thinking {
                    write!(
                        writer,
                        "{}{}{}",
                        self.colors.dim, self.colors.italic, remaining
                    )?;
                } else {
                    write!(writer, "{}", remaining)?;
                }
                current_pos = processing_text.len();
            }
        }

        // Apply appropriate styling reset if needed
        if state.in_thinking {
            write!(writer, "{}", self.colors.reset)?;
        }

        writer.flush()?;
        Ok(())
    }
}
