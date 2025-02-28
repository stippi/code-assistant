use super::{UIError, UIMessage, UserInterface};
use async_trait::async_trait;
use crossterm::{
    style::{self, Color, Stylize},
    terminal::{self},
};
use rustyline::{error::ReadlineError, history::DefaultHistory, Config, Editor};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

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
    state: Arc<Mutex<FormattingState>>,
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
            state: Arc::new(Mutex::new(FormattingState {
                buffer: String::new(),
                in_thinking: false,
                in_tool: false,
                in_param: false,
                tool_name: String::new(),
            })),
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
            state: Arc::new(Mutex::new(FormattingState {
                buffer: String::new(),
                in_thinking: false,
                in_tool: false,
                in_param: false,
                tool_name: String::new(),
            })),
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
                "─".repeat(1).with(border_color),
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

        result.push_str(&format!("{}╮\n", "─".repeat(1).with(border_color)));

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
                // Format questions with a frame
                let formatted_question = self.frame_content(&msg, Some("Question"), Color::Cyan);
                self.write_line(&formatted_question).await?
            }
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
            // Check at most last 40 chars
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
                        // Format thinking text with crossterm
                        let styled_text = pre_tag_text.dark_grey().italic();
                        write!(writer, "{}", styled_text)?;
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
                        write!(writer, "{}", style::ResetColor)?;

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

                            // Output tool start with a clean format
                            if !state.in_thinking {
                                // Bullet point and tool name in bold blue
                                write!(writer, "\n• {}", format!("{}", tool_name).bold().blue())?;
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
                            // Get param name if available
                            let param_name = if end_pos > 7 {
                                &tag_slice[7..end_pos]
                            } else {
                                "param"
                            };

                            // Format parameter start with indentation
                            if !state.in_thinking && state.in_tool {
                                write!(writer, "  {}: ", param_name.cyan())?;
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
                                "{}",
                                processing_text[absolute_tag_pos..absolute_tag_pos + 1]
                                    .dark_grey()
                                    .italic()
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
                    write!(writer, "{}", remaining.dark_grey().italic())?;
                } else {
                    write!(writer, "{}", remaining)?;
                }
                current_pos = processing_text.len();
            }
        }

        // Apply appropriate styling reset if needed
        if state.in_thinking {
            write!(writer, "{}", style::ResetColor)?;
        }

        writer.flush()?;
        Ok(())
    }
}
