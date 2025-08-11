use super::{DisplayFragment, ToolStatus, UIError, UiEvent, UserInterface};
use async_trait::async_trait;
use crossterm::style::{self, Color, Stylize};
use rustyline::{error::ReadlineError, history::DefaultHistory, Config, Editor};
use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::sleep;

#[derive(Debug, Clone)]
enum SpinnerState {
    None,
    Loading {
        frame: usize,
    },
    RateLimit {
        seconds_remaining: u64,
        frame: usize,
    },
}

#[derive(Clone)]
pub struct TerminalUI {
    // For line editor - using just the default implementation without custom helper
    line_editor: Arc<Mutex<Editor<(), DefaultHistory>>>,
    // In production code, this isn't used
    writer: Option<Arc<Mutex<Box<dyn Write + Send>>>>,
    // Track tool parameters to avoid repeating parameter names during streaming
    tool_parameters: Arc<Mutex<HashMap<String, HashMap<String, String>>>>, // tool_id -> param_name -> param_value
    // Track which parameters have been displayed
    displayed_parameters: Arc<Mutex<HashMap<String, HashSet<String>>>>, // tool_id -> set of param names
    // Spinner state for loading and rate limiting
    spinner_state: Arc<Mutex<SpinnerState>>,
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
            tool_parameters: Arc::new(Mutex::new(HashMap::new())),
            displayed_parameters: Arc::new(Mutex::new(HashMap::new())),
            spinner_state: Arc::new(Mutex::new(SpinnerState::None)),
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
            tool_parameters: Arc::new(Mutex::new(HashMap::new())),
            displayed_parameters: Arc::new(Mutex::new(HashMap::new())),
            spinner_state: Arc::new(Mutex::new(SpinnerState::None)),
        }
    }

    async fn write_line(&self, s: &str) -> Result<(), UIError> {
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{s}")?;
        Ok(())
    }
}

#[async_trait]
impl UserInterface for TerminalUI {
    async fn send_event(&self, event: UiEvent) -> Result<(), UIError> {
        match event {
            UiEvent::DisplayUserInput {
                content,
                attachments,
            } => {
                // Display user input with attachments
                let mut formatted = format!("{} {}", ">".with(Color::Green), content);
                if !attachments.is_empty() {
                    formatted.push_str(&format!(" [with {} attachment(s)]", attachments.len()));
                }
                self.write_line(&formatted).await?
            }
            UiEvent::UpdateToolStatus {
                tool_id: _,
                status,
                message: Some(msg),
                output: _,
            } => {
                // For terminal UI, we just print a status message if provided
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
            UiEvent::UpdateToolStatus {
                tool_id: _,
                status: _,
                message: None,
                output: _,
            } => {
                // No message to display
            }
            UiEvent::StartTool { name: _, id } => {
                // Clear any previous parameters for this tool
                self.tool_parameters.lock().unwrap().remove(&id);
                self.displayed_parameters.lock().unwrap().remove(&id);
            }
            UiEvent::UpdateToolParameter {
                tool_id,
                name,
                value,
            } => {
                // Update the parameter value
                {
                    let mut params = self.tool_parameters.lock().unwrap();
                    let tool_params = params.entry(tool_id.clone()).or_insert_with(HashMap::new);
                    tool_params.insert(name.clone(), value.clone());
                }

                // Check if we need to display the parameter name (first time for this parameter)
                let mut displayed = self.displayed_parameters.lock().unwrap();
                let tool_displayed = displayed
                    .entry(tool_id.clone())
                    .or_insert_with(HashSet::new);

                let mut stdout = io::stdout().lock();
                let writer: &mut dyn Write = if let Some(w) = &self.writer {
                    &mut *w.lock().unwrap()
                } else {
                    &mut stdout
                };

                if !tool_displayed.contains(&name) {
                    // First time seeing this parameter, display the name
                    write!(writer, "  {}: ", name.clone().cyan())?;
                    tool_displayed.insert(name.clone());
                } else {
                    // Parameter already displayed, just update the value in place
                    // For terminal, we can't easily update in place, so we'll just append
                    // This is a limitation of terminal UI vs GUI
                }

                // Display the current value
                write!(writer, "{value}")?;
                writer.flush()?;
            }
            UiEvent::EndTool { id } => {
                // Tool ended, add a newline for better formatting
                let mut stdout = io::stdout().lock();
                let writer: &mut dyn Write = if let Some(w) = &self.writer {
                    &mut *w.lock().unwrap()
                } else {
                    &mut stdout
                };
                writeln!(writer)?;

                // Clean up tracking for this tool
                self.tool_parameters.lock().unwrap().remove(&id);
                self.displayed_parameters.lock().unwrap().remove(&id);
            }
            UiEvent::StreamingStarted(_request_id) => {
                // Start the loading spinner
                self.start_loading_spinner();
            }
            UiEvent::StreamingStopped {
                id: _request_id,
                cancelled,
            } => {
                // Stop the spinner first
                self.stop_spinner();

                // Only display if cancelled, completion is implied
                if cancelled {
                    self.write_line(&"❌ Request cancelled".red().to_string())
                        .await?;
                }
                // Add a blank line and show the prompt for better UX
                self.write_line("").await?; // Add some space
                self.show_prompt();
            }
            UiEvent::UpdateMemory { memory: _ } => {
                // Terminal UI doesn't display memory visually, so this is a no-op
            }
            // Terminal UI ignores other events (they're for GPUI)
            _ => {}
        }
        Ok(())
    }

    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError> {
        // Stop spinner when we start receiving content
        {
            let mut state = self.spinner_state.lock().unwrap();
            if !matches!(*state, SpinnerState::None) {
                *state = SpinnerState::None;

                // Clear spinner line only for non-test mode and ensure we're on a fresh line
                if self.writer.is_none() {
                    drop(state); // Release lock before I/O
                    let mut stdout = io::stdout().lock();
                    write!(stdout, "\r\x1b[K").unwrap(); // Clear the spinner line
                    let _ = stdout.flush();
                }
            }
        }

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
                write!(writer, "{text}")?;
            }
            DisplayFragment::ThinkingText(text) => {
                // Format thinking text with crossterm
                let styled_text = text.clone().dark_grey().italic();
                write!(writer, "{styled_text}")?;
            }
            DisplayFragment::ToolName { name, .. } => {
                // Format tool name in bold blue with bullet point
                write!(writer, "\n• {}", name.to_string().bold().blue())?;
            }
            DisplayFragment::ToolParameter {
                name,
                value,
                tool_id,
            } => {
                // Handle parameter streaming properly
                // Update the parameter value in our tracking
                {
                    let mut params = self.tool_parameters.lock().unwrap();
                    let tool_params = params.entry(tool_id.clone()).or_insert_with(HashMap::new);
                    tool_params.insert(name.clone(), value.clone());
                }

                // Check if we need to display the parameter name (first time for this parameter)
                let mut displayed = self.displayed_parameters.lock().unwrap();
                let tool_displayed = displayed
                    .entry(tool_id.clone())
                    .or_insert_with(HashSet::new);

                if !tool_displayed.contains(name) {
                    // First time seeing this parameter, display the name
                    write!(writer, "  {}: ", name.clone().cyan())?;
                    tool_displayed.insert(name.clone());
                }

                // Always display the new chunk of the value
                write!(writer, "{value}")?;
            }
            DisplayFragment::ToolEnd { .. } => {
                // No special formatting needed at tool end
            }
            DisplayFragment::Image { media_type, .. } => {
                // Display image placeholder in terminal (can't show actual images)
                write!(writer, "[Image: {}]", media_type.clone().yellow())?;
            }
        }

        writer.flush()?;
        Ok(())
    }

    fn should_streaming_continue(&self) -> bool {
        // Terminal UI always continues streaming (no stop functionality)
        true
    }

    fn notify_rate_limit(&self, seconds_remaining: u64) {
        // Update spinner state to show rate limit countdown
        let was_none = {
            let mut state = self.spinner_state.lock().unwrap();
            let was_none = matches!(*state, SpinnerState::None);
            *state = SpinnerState::RateLimit {
                seconds_remaining,
                frame: 0,
            };
            was_none
        };

        // If spinner wasn't running, start it
        if was_none {
            self.start_loading_spinner();
        }
    }

    fn clear_rate_limit(&self) {
        // Stop the rate limit spinner
        self.stop_spinner();
    }
}

impl TerminalUI {
    /// Spinner characters for loading animation
    const SPINNER_CHARS: &'static [char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

    /// Start the loading spinner
    fn start_loading_spinner(&self) {
        // Only start if not already spinning
        {
            let state = self.spinner_state.lock().unwrap();
            if !matches!(*state, SpinnerState::None) {
                return; // Already spinning
            }
        }

        // Set initial state
        {
            let mut state = self.spinner_state.lock().unwrap();
            *state = SpinnerState::Loading { frame: 0 };
        }

        // Start the spinner animation task
        let spinner_state = self.spinner_state.clone();
        let writer = self.writer.clone();

        tokio::spawn(async move {
            loop {
                // Check current state and update display
                let should_continue = {
                    let mut state = spinner_state.lock().unwrap();
                    match &mut *state {
                        SpinnerState::Loading { frame } => {
                            Self::update_spinner_display(
                                &SpinnerState::Loading { frame: *frame },
                                &writer,
                            );
                            *frame = (*frame + 1) % Self::SPINNER_CHARS.len();
                            true
                        }
                        SpinnerState::RateLimit {
                            seconds_remaining,
                            frame,
                        } => {
                            Self::update_spinner_display(
                                &SpinnerState::RateLimit {
                                    seconds_remaining: *seconds_remaining,
                                    frame: *frame,
                                },
                                &writer,
                            );
                            *frame = (*frame + 1) % Self::SPINNER_CHARS.len();
                            true
                        }
                        SpinnerState::None => false,
                    }
                };

                if !should_continue {
                    break;
                }

                // Wait before next frame
                sleep(Duration::from_millis(100)).await;
            }
        });
    }

    /// Stop the spinner and clear the line
    fn stop_spinner(&self) {
        {
            let mut state = self.spinner_state.lock().unwrap();
            if matches!(*state, SpinnerState::None) {
                return; // Already stopped
            }
            *state = SpinnerState::None;
        }

        // Clear the spinner line only in non-test mode
        if self.writer.is_none() {
            self.clear_spinner_line();
        }
    }

    /// Update spinner display based on current state
    fn update_spinner_display(
        state: &SpinnerState,
        writer: &Option<Arc<Mutex<Box<dyn Write + Send>>>>,
    ) {
        let mut stdout = io::stdout().lock();
        let writer_ref: &mut dyn Write = if let Some(w) = writer {
            &mut *w.lock().unwrap()
        } else {
            &mut stdout
        };

        match state {
            SpinnerState::Loading { frame, .. } => {
                let spinner_char = Self::SPINNER_CHARS[*frame];
                write!(
                    writer_ref,
                    "\r{} Thinking...",
                    spinner_char.to_string().cyan()
                )
                .unwrap();
            }
            SpinnerState::RateLimit {
                seconds_remaining,
                frame,
            } => {
                let spinner_char = Self::SPINNER_CHARS[*frame];
                write!(
                    writer_ref,
                    "\r{} Rate limited - retrying in {}s...",
                    spinner_char.to_string().yellow(),
                    seconds_remaining
                )
                .unwrap();
            }
            SpinnerState::None => {}
        }

        let _ = writer_ref.flush();
    }

    /// Clear the spinner line
    fn clear_spinner_line(&self) {
        let mut stdout = io::stdout().lock();
        let writer: &mut dyn Write = if let Some(w) = &self.writer {
            &mut *w.lock().unwrap()
        } else {
            &mut stdout
        };

        // Clear the current line and move cursor to beginning, then add newline
        write!(writer, "\r\x1b[K").unwrap();
        let _ = writer.flush();
    }

    /// Show the input prompt without waiting for input
    pub fn show_prompt(&self) {
        let prompt = format!("{} ", ">".with(Color::Green));

        let mut stdout = io::stdout().lock();
        let writer: &mut dyn Write = if let Some(w) = &self.writer {
            &mut *w.lock().unwrap()
        } else {
            &mut stdout
        };

        let _ = write!(writer, "{prompt}");
        let _ = writer.flush(); // Make sure it appears immediately
    }

    /// Get input from the user (terminal-specific method)
    pub async fn get_input(&self) -> Result<String, UIError> {
        // Access the editor
        let mut editor = self.line_editor.lock().unwrap();

        // Set a prompt with color
        let colored_prompt = format!("{}{} ", ">".with(Color::Green), style::ResetColor);

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
            Err(e) => Err(UIError::IOError(io::Error::other(format!(
                "Input error: {e}"
            )))),
        }
    }
}
