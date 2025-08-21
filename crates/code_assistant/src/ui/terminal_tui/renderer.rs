use anyhow::Result;
use ratatui::{
    backend::CrosstermBackend,
    layout::Position,
    prelude::*,
    widgets::{Block, Borders, Paragraph, Wrap},
    Terminal, TerminalOptions, Viewport,
};
use std::io;
use tui_markdown as md;
use tui_textarea::TextArea;

use super::message::{LiveMessage, MessageBlock, PlainTextBlock, ThinkingBlock, ToolUseBlock};
use crate::ui::ToolStatus;
use std::time::Instant;

/// Spinner state for loading indication
#[derive(Debug, Clone)]
pub enum SpinnerState {
    Hidden,
    Loading {
        start_time: Instant,
    },
    RateLimit {
        start_time: Instant,
        seconds_remaining: u64,
    },
}

impl SpinnerState {
    fn get_spinner_char(&self) -> Option<(char, Color)> {
        match self {
            SpinnerState::Hidden => None,
            SpinnerState::Loading { start_time } => {
                let braille_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
                let elapsed_ms = start_time.elapsed().as_millis();
                let index = (elapsed_ms / 100) % braille_chars.len() as u128;
                Some((braille_chars[index as usize], Color::Blue))
            }
            SpinnerState::RateLimit { start_time, .. } => {
                let braille_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
                let elapsed_ms = start_time.elapsed().as_millis();
                let index = (elapsed_ms / 100) % braille_chars.len() as u128;
                Some((braille_chars[index as usize], Color::LightRed))
            }
        }
    }

    fn get_status_text(&self) -> Option<String> {
        match self {
            SpinnerState::Hidden => None,
            SpinnerState::Loading { .. } => None,
            SpinnerState::RateLimit {
                seconds_remaining, ..
            } => Some(format!("Rate limited ({seconds_remaining}s)")),
        }
    }
}

/// Handles the terminal display and rendering using ratatui
pub struct TerminalRenderer {
    pub terminal: Terminal<CrosstermBackend<io::Stdout>>,
    /// Finalized messages (as complete message structures)
    pub finalized_messages: Vec<LiveMessage>,
    /// Current live message being streamed
    pub live_message: Option<LiveMessage>,
    /// Optional pending user message (displayed between input and live content while streaming)
    pending_user_message: Option<String>,
    /// Last computed overflow (how many rows have been promoted so far); used to promote only deltas
    pub last_overflow: u16,
    /// Maximum rows for input area (including 1 for content min + border)
    pub max_input_rows: u16,
    /// Counter for generating message IDs
    next_message_id: u64,
    /// Spinner state for loading indication
    spinner_state: SpinnerState,
}

impl TerminalRenderer {
    pub fn new() -> Result<Self> {
        let terminal = Self::create_terminal()?;

        Ok(Self {
            terminal,
            finalized_messages: Vec::new(),
            live_message: None,
            pending_user_message: None,
            last_overflow: 0,
            max_input_rows: 5, // max input height (content lines + border line)
            next_message_id: 1,
            spinner_state: SpinnerState::Hidden,
        })
    }

    // Create a terminal with inline viewport height equal to current terminal height
    fn create_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
        let (_w, h) = ratatui::crossterm::terminal::size()?;
        let backend = CrosstermBackend::new(io::stdout());
        let options = TerminalOptions {
            viewport: Viewport::Inline(h),
        };
        Terminal::with_options(backend, options).map_err(Into::into)
    }

    /// Setup terminal for ratatui usage
    pub fn setup_terminal(&mut self) -> Result<()> {
        ratatui::crossterm::terminal::enable_raw_mode()?;
        Ok(())
    }

    /// Cleanup terminal when exiting
    pub fn cleanup_terminal(&mut self) -> Result<()> {
        ratatui::crossterm::terminal::disable_raw_mode()?;
        Ok(())
    }

    /// Update terminal size and adjust viewport (recreate Terminal with new Inline height)
    pub fn update_size(&mut self, _input_height: u16) -> Result<()> {
        self.terminal = Self::create_terminal()?;
        Ok(())
    }

    /// Start a new message (called on StreamingStarted)
    pub fn start_new_message(&mut self) {
        // Show loading spinner
        self.spinner_state = SpinnerState::Loading {
            start_time: Instant::now(),
        };
        // Finalize current message if any
        if let Some(mut current_message) = self.live_message.take() {
            current_message.finalized = true;
            if current_message.has_content() {
                self.finalized_messages.push(current_message);
            }
        }

        // Start new live message
        let message_id = format!("msg_{}", self.next_message_id);
        self.next_message_id += 1;
        self.live_message = Some(LiveMessage::new(message_id));
        self.last_overflow = 0;
    }

    /// Start a new plain text block within the current message
    pub fn start_plain_text_block(&mut self) {
        if let Some(ref mut message) = self.live_message {
            message.add_block(MessageBlock::PlainText(PlainTextBlock::new()));
        }
    }

    /// Start a new thinking block within the current message
    pub fn start_thinking_block(&mut self) {
        if let Some(ref mut message) = self.live_message {
            message.add_block(MessageBlock::Thinking(ThinkingBlock::new()));
        }
    }

    /// Start a new tool use block within the current message
    pub fn start_tool_use_block(&mut self, name: String, id: String) {
        // Hide spinner when first content arrives
        self.hide_loading_spinner_if_active();
        if let Some(ref mut message) = self.live_message {
            message.add_block(MessageBlock::ToolUse(ToolUseBlock::new(name, id)));
        }
    }

    /// Legacy method for backward compatibility - starts a plain text block
    pub fn start_live_block(&mut self) {
        // Ensure we have a live message
        if self.live_message.is_none() {
            self.start_new_message();
        }
        self.start_plain_text_block();
    }

    /// Set or unset a pending user message (displayed while streaming)
    pub fn set_pending_user_message(&mut self, message: Option<String>) {
        self.pending_user_message = message;
    }

    /// Append text to the last block in the current message
    pub fn append_to_live_block(&mut self, text: &str) {
        // Hide spinner when first content arrives
        self.hide_loading_spinner_if_active();
        if let Some(ref mut message) = self.live_message {
            if let Some(last_block) = message.get_last_block_mut() {
                last_block.append_content(text);
            }
        }
    }

    /// Add or update a tool parameter in the current message
    pub fn add_or_update_tool_parameter(&mut self, tool_id: &str, name: String, value: String) {
        if let Some(ref mut message) = self.live_message {
            if let Some(tool_block) = message.get_tool_block_mut(tool_id) {
                tool_block.add_or_update_parameter(name, value);
            }
        }
    }

    /// Update tool status in the current message
    pub fn update_tool_status(
        &mut self,
        tool_id: &str,
        status: ToolStatus,
        message: Option<String>,
        output: Option<String>,
    ) {
        if let Some(ref mut live_message) = self.live_message {
            if let Some(tool_block) = live_message.get_tool_block_mut(tool_id) {
                tool_block.status = status;
                tool_block.status_message = message;
                tool_block.output = output;
            }
        }
    }

    /// Finalize the current live message
    pub fn finalize_live_message(&mut self) -> Result<()> {
        if let Some(mut message) = self.live_message.take() {
            message.finalized = true;
            if message.has_content() {
                self.finalized_messages.push(message);
            }
        }
        self.last_overflow = 0;
        Ok(())
    }

    /// Legacy method - finalize current message
    pub fn finalize_live_block(&mut self) -> Result<()> {
        self.finalize_live_message()
    }

    /// Add a user message as finalized message and clear any pending user message
    pub fn add_user_message(&mut self, content: &str) -> Result<()> {
        // Create a finalized message with a single plain text block
        let message_id = format!("user_{}", self.next_message_id);
        self.next_message_id += 1;

        let mut user_message = LiveMessage::new(message_id);
        let mut text_block = PlainTextBlock::new();
        text_block.content = content.to_string();
        user_message.add_block(MessageBlock::PlainText(text_block));
        user_message.finalized = true;

        self.finalized_messages.push(user_message);
        self.pending_user_message = None; // Clear pending message when it becomes finalized
        Ok(())
    }

    /// Clear all messages and reset state
    pub fn clear_all_messages(&mut self) {
        self.finalized_messages.clear();
        self.live_message = None;
        self.last_overflow = 0;
        self.spinner_state = SpinnerState::Hidden;
    }

    /// Show rate limit spinner with countdown
    pub fn show_rate_limit_spinner(&mut self, seconds_remaining: u64) {
        self.spinner_state = SpinnerState::RateLimit {
            start_time: Instant::now(),
            seconds_remaining,
        };
    }

    /// Hide spinner
    pub fn hide_spinner(&mut self) {
        self.spinner_state = SpinnerState::Hidden;
    }

    /// Hide spinner if it's currently showing loading state
    pub fn hide_loading_spinner_if_active(&mut self) {
        if matches!(self.spinner_state, SpinnerState::Loading { .. }) {
            self.spinner_state = SpinnerState::Hidden;
        }
    }

    /// Render the complete UI: composed finalized content + live content + 1-line gap + input
    pub fn render(&mut self, textarea: &TextArea) -> Result<()> {
        // Phase 1: compute layout and promotion outside of draw
        let term_size = self.terminal.size()?;
        let input_height = self.calculate_input_height(textarea);
        let available = term_size.height.saturating_sub(input_height);
        let width = term_size.width;

        // Compose scratch buffer: render 1 blank line (gap), then live_message, then finalized tail above
        let headroom: u16 = 200; // keep small to reduce work per frame
        let scratch_height = available.saturating_add(headroom).max(available);
        let mut scratch = Buffer::empty(Rect::new(0, 0, width, scratch_height));

        // We pack from bottom up: bottom = gap, above it pending user msg (if any), above it live, above that finalized tail
        let mut cursor_y = scratch_height; // one past last line

        // Reserve one blank line as gap above input (at the very bottom)
        cursor_y = cursor_y.saturating_sub(1);

        // Reserve space for pending user message if present
        let pending_height = if let Some(ref pending_msg) = self.pending_user_message {
            let rendered_height = self.render_message_content_to_buffer(
                pending_msg,
                &mut scratch,
                &mut cursor_y,
                width,
            );
            // Add a small gap above pending message
            cursor_y = cursor_y.saturating_sub(1);
            rendered_height + 1
        } else {
            0
        };

        // 1) Render spinner if active (closest to input)
        if let Some((spinner_char, spinner_color)) = self.spinner_state.get_spinner_char() {
            if cursor_y > 0 {
                cursor_y = cursor_y.saturating_sub(1);

                // Render spinner character
                scratch.set_string(
                    0,
                    cursor_y,
                    spinner_char.to_string(),
                    Style::default().fg(spinner_color),
                );

                // Render status text if present
                if let Some(status_text) = self.spinner_state.get_status_text() {
                    scratch.set_string(
                        2,
                        cursor_y,
                        &status_text,
                        Style::default().fg(Color::LightRed),
                    );
                }

                // Add gap after spinner
                cursor_y = cursor_y.saturating_sub(1);
            }
        }

        // 2) Render current live message (so it is closest to the input)
        if let Some(ref live_message) = self.live_message {
            if live_message.has_content() && cursor_y > 0 {
                self.render_message_to_buffer(live_message, &mut scratch, &mut cursor_y, width);
                // Add gap after live message
                cursor_y = cursor_y.saturating_sub(1);
            }
        }

        // 3) Render finalized messages from newest to oldest above live until we filled enough
        for message in self.finalized_messages.iter().rev() {
            if cursor_y == 0 {
                break;
            }
            // stop if we already filled enough (available + headroom)
            let filled = scratch_height - cursor_y;
            if filled >= available + headroom {
                break;
            }

            self.render_message_to_buffer(message, &mut scratch, &mut cursor_y, width);
            // Add gap after each message
            cursor_y = cursor_y.saturating_sub(1);
        }

        // Now composed content occupies rows [cursor_y .. scratch_height)
        let total_height = scratch_height.saturating_sub(cursor_y);
        let overflow = total_height.saturating_sub(available);

        // Promote only the delta that has not yet been promoted
        if overflow > self.last_overflow {
            let new_to_promote = overflow - self.last_overflow;
            let promote_start = cursor_y + self.last_overflow;
            let term_width = width;
            self.terminal
                .insert_before(new_to_promote, |buf: &mut Buffer| {
                    for y in 0..new_to_promote {
                        for x in 0..term_width {
                            let row = promote_start + y;
                            let src = scratch
                                .cell((x, row))
                                .cloned()
                                .unwrap_or_else(ratatui::buffer::Cell::default);
                            if let Some(dst) = buf.cell_mut((x, y)) {
                                *dst = src;
                            }
                        }
                    }
                })?;
            self.last_overflow = overflow;
        }

        // Phase 2: draw bottom `available` rows, pending message, and input
        self.terminal.draw(|f| {
            let full = f.area();

            let [content_area, pending_area, input_area] = Layout::vertical([
                Constraint::Min(1),
                Constraint::Length(pending_height),
                Constraint::Length(input_height),
            ])
            .areas(full);

            let visible_total = total_height.min(content_area.height);
            let top_blank = content_area.height - visible_total; // rows to leave blank at top
            let visible_start = cursor_y.saturating_add(overflow);
            let dst = f.buffer_mut();

            // Top blank area (if any)
            for y in 0..top_blank {
                // clear line
                for x in 0..content_area.width {
                    if let Some(cell) = dst.cell_mut((content_area.x + x, content_area.y + y)) {
                        *cell = ratatui::buffer::Cell::default();
                    }
                }
            }

            // Copy visible content aligned at the bottom of content_area
            for y in 0..visible_total {
                for x in 0..content_area.width {
                    let src_row = visible_start + y;
                    let src = scratch
                        .cell((x, src_row))
                        .cloned()
                        .unwrap_or_else(ratatui::buffer::Cell::default);
                    if let Some(dst_cell) =
                        dst.cell_mut((content_area.x + x, content_area.y + top_blank + y))
                    {
                        *dst_cell = src;
                    }
                }
            }

            // Render pending user message if present
            if let Some(ref pending_msg) = self.pending_user_message {
                Self::render_pending_message(f, pending_area, pending_msg);
            }

            // Render input area (block + textarea)
            Self::render_input_area_static(f, input_area, textarea);
        })?;

        Ok(())
    }

    /// Render a message to the scratch buffer, updating cursor_y
    fn render_message_to_buffer(
        &self,
        message: &LiveMessage,
        scratch: &mut Buffer,
        cursor_y: &mut u16,
        width: u16,
    ) {
        // Render blocks from last to first (bottom to top)
        for block in message.blocks.iter().rev() {
            if *cursor_y == 0 {
                break;
            }

            let widget = block.create_widget();
            let block_height = widget.calculate_height(width).min(*cursor_y);

            if block_height > 0 {
                let area = Rect::new(
                    0,
                    cursor_y.saturating_sub(block_height),
                    width,
                    block_height,
                );
                widget.render(area, scratch);
                *cursor_y = cursor_y.saturating_sub(block_height);

                // Add one line gap between blocks within a message
                *cursor_y = cursor_y.saturating_sub(1);
            }
        }
    }

    /// Render markdown content to buffer (for pending messages)
    fn render_message_content_to_buffer(
        &self,
        content: &str,
        scratch: &mut Buffer,
        cursor_y: &mut u16,
        width: u16,
    ) -> u16 {
        if content.trim().is_empty() || *cursor_y == 0 {
            return 0;
        }

        let text = md::from_str(content);
        let para_for_measure = Paragraph::new(text.clone()).wrap(Wrap { trim: false });

        // Only allocate tmp up to remaining space to keep it cheap
        let max_h = (*cursor_y).clamp(1, 500); // cap to 500 to avoid huge temps
        let mut tmp = Buffer::empty(Rect::new(0, 0, width, max_h));
        para_for_measure.render(Rect::new(0, 0, width, max_h), &mut tmp);

        let mut used = 0u16;
        'scan: for y in (0..max_h).rev() {
            let mut row_empty = true;
            for x in 0..width {
                let c = tmp.cell((x, y)).expect("cell in tmp buffer");
                if !c.symbol().is_empty() && c.symbol() != " " {
                    row_empty = false;
                    break;
                }
            }
            if !row_empty {
                used = y + 1;
                break 'scan;
            }
        }

        if used == 0 {
            return 0;
        }

        let h = used.min(*cursor_y);
        if h == 0 {
            return 0;
        }

        // render into destination aligned at bottom
        let area = Rect::new(0, cursor_y.saturating_sub(h), width, h);
        let para_for_draw = Paragraph::new(text).wrap(Wrap { trim: false });
        para_for_draw.render(area, scratch);
        *cursor_y = cursor_y.saturating_sub(h);
        h
    }

    /// Calculate the height needed for the input area based on textarea content
    pub fn calculate_input_height(&self, textarea: &TextArea) -> u16 {
        let lines = textarea.lines().len() as u16;
        // Reserve at least 2 lines (1 for border + 1 for content)
        lines.clamp(2, self.max_input_rows + 1)
    }

    /// Render the input area with textarea (static version)
    fn render_input_area_static(f: &mut Frame, area: Rect, textarea: &TextArea) {
        // Create a block with border for the input area
        let input_block = Block::default()
            .borders(Borders::TOP)
            .title("Input (Enter=send, Shift+Enter=newline, Ctrl+C=quit)");

        // Render the textarea widget inside the block
        let inner_area = input_block.inner(area);
        f.render_widget(input_block, area);
        f.render_widget(textarea, inner_area);

        // Set cursor position for the textarea
        let cursor_pos = textarea.cursor();
        let cursor_x = inner_area.x + cursor_pos.1 as u16;
        let cursor_y = inner_area.y + cursor_pos.0 as u16;
        f.set_cursor_position(Position::new(cursor_x, cursor_y));
    }

    /// Render pending user message with dimmed and italic styling
    fn render_pending_message(f: &mut Frame, area: Rect, message: &str) {
        if area.height == 0 {
            return;
        }

        let text = md::from_str(message);
        let paragraph = Paragraph::new(text)
            .style(
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )
            .wrap(Wrap { trim: false });

        f.render_widget(paragraph, area);
    }
}
