use anyhow::Result;
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::Position,
    prelude::*,
    widgets::{Block, Borders, Paragraph, Wrap},
    Terminal, TerminalOptions, Viewport,
};
use std::io;
use tui_markdown as md;
use tui_textarea::TextArea;

use super::message::{LiveMessage, MessageBlock, PlainTextBlock, ToolUseBlock};
use crate::types::{PlanItemStatus, PlanState};
use crate::ui::ToolStatus;
use std::time::Instant;
use tracing::debug;

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

enum StatusKind {
    Info,
    Plan,
    Pending,
}

struct StatusEntry {
    kind: StatusKind,
    content: String,
    height: u16,
}

/// Handles the terminal display and rendering using ratatui
pub struct TerminalRenderer<B: Backend> {
    pub terminal: Terminal<B>,
    /// Factory function to create new terminal instances (used for resizing)
    terminal_factory: Box<dyn Fn() -> Result<Terminal<B>> + Send + Sync>,
    /// Finalized messages (as complete message structures)
    pub finalized_messages: Vec<LiveMessage>,
    /// Current live message being streamed
    pub live_message: Option<LiveMessage>,
    /// Optional pending user message (displayed between input and live content while streaming)
    pending_user_message: Option<String>,
    /// Current error message to display
    current_error: Option<String>,
    /// Current info message to display
    info_message: Option<String>,
    /// Latest plan state received from the agent
    plan_state: Option<PlanState>,
    /// Whether to render the expanded plan view
    plan_expanded: bool,
    /// Last computed overflow (how many rows have been promoted so far); used to promote only deltas
    pub last_overflow: u16,
    /// Maximum rows for input area (including 1 for content min + border)
    pub max_input_rows: u16,
    /// Spinner state for loading indication
    spinner_state: SpinnerState,
}

/// Type alias for the production terminal renderer
pub type ProductionTerminalRenderer = TerminalRenderer<CrosstermBackend<io::Stdout>>;

impl<B: Backend> TerminalRenderer<B> {
    pub fn with_factory<F>(factory: F) -> Result<Self>
    where
        F: Fn() -> Result<Terminal<B>> + Send + Sync + 'static,
    {
        let terminal = factory()?;
        Ok(Self {
            terminal,
            terminal_factory: Box::new(factory),
            finalized_messages: Vec::new(),
            live_message: None,
            pending_user_message: None,
            current_error: None,
            info_message: None,
            plan_state: None,
            plan_expanded: false,
            last_overflow: 0,
            max_input_rows: 5, // max input height (content lines + border line)
            spinner_state: SpinnerState::Hidden,
        })
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

    /// Update terminal size and adjust viewport (recreate terminal using factory)
    pub fn update_size(&mut self, _input_height: u16) -> Result<()> {
        self.terminal = (self.terminal_factory)()?;
        Ok(())
    }

    /// Start a new message (called on StreamingStarted)
    pub fn start_new_message(&mut self, _request_id: u64) {
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
        self.live_message = Some(LiveMessage::new());
        self.last_overflow = 0;
    }

    /// Start a new tool use block within the current message
    pub fn start_tool_use_block(&mut self, name: String, id: String) {
        // Hide spinner when first content arrives
        self.hide_loading_spinner_if_active();

        let live_message = self
            .live_message
            .as_mut()
            .expect("start_tool_use_block called without an active live message");

        live_message.add_block(MessageBlock::ToolUse(ToolUseBlock::new(name, id)));
    }

    /// Add a context compaction marker block
    pub fn add_context_compaction_block(
        &mut self,
        compaction_number: u32,
        messages_archived: usize,
        context_size_before: u32,
        summary: String,
    ) {
        // Hide spinner when first content arrives
        self.hide_loading_spinner_if_active();

        let live_message = self
            .live_message
            .as_mut()
            .expect("add_context_compaction_block called without an active live message");

        live_message.add_block(MessageBlock::ContextCompaction(
            super::message::ContextCompactionBlock::new(
                compaction_number,
                messages_archived,
                context_size_before,
                summary,
            ),
        ));
    }

    /// Ensure the last block in the live message is of the specified type
    /// If not, append a new block of that type
    /// Returns true if a new block was created, false if the last block was already the right type
    pub fn ensure_last_block_type(&mut self, block: MessageBlock) -> bool {
        let live_message = self.live_message.as_mut()
            .expect("ensure_last_block_type called without an active live message - call start_new_message first");

        // Check if we need a new block of the specified type
        let needs_new_block = match live_message.blocks.last() {
            Some(last_block) => {
                std::mem::discriminant(last_block) != std::mem::discriminant(&block)
            }
            None => true, // No blocks, need new block
        };

        if needs_new_block {
            live_message.add_block(block);
        }

        needs_new_block
    }

    /// Set or unset a pending user message (displayed while streaming)
    pub fn set_pending_user_message(&mut self, message: Option<String>) {
        self.pending_user_message = message;
    }

    /// Update the stored plan state for rendering
    pub fn set_plan_state(&mut self, plan: Option<PlanState>) {
        if let Some(ref plan_state) = plan {
            debug!(
                "renderer::set_plan_state received {} entries (expanded currently {})",
                plan_state.entries.len(),
                self.plan_expanded
            );
        } else {
            debug!("renderer::set_plan_state clearing plan state");
        }
        self.plan_state = plan;
    }

    /// Toggle whether the expanded plan view should be rendered
    pub fn set_plan_expanded(&mut self, expanded: bool) {
        self.plan_expanded = expanded;
    }

    /// Append text to the last block in the current message
    pub fn append_to_live_block(&mut self, text: &str) {
        // Hide spinner when first content arrives
        self.hide_loading_spinner_if_active();

        let live_message = self
            .live_message
            .as_mut()
            .expect("append_to_live_block called without an active live message");

        if let Some(last_block) = live_message.get_last_block_mut() {
            last_block.append_content(text);
        }
    }

    /// Add or update a tool parameter in the current message
    pub fn add_or_update_tool_parameter(&mut self, tool_id: &str, name: String, value: String) {
        let live_message = self
            .live_message
            .as_mut()
            .expect("add_or_update_tool_parameter called without an active live message");

        if let Some(tool_block) = live_message.get_tool_block_mut(tool_id) {
            tool_block.add_or_update_parameter(name, value);
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
        let live_message = self
            .live_message
            .as_mut()
            .expect("update_tool_status called without an active live message");

        if let Some(tool_block) = live_message.get_tool_block_mut(tool_id) {
            tool_block.status = status;
            tool_block.status_message = message;
            tool_block.output = output;
        }
    }

    /// Add a user message as finalized message and clear any pending user message
    pub fn add_user_message(&mut self, content: &str) -> Result<()> {
        // Create a finalized message with a single plain text block
        let mut user_message = LiveMessage::new();
        let mut text_block = PlainTextBlock::new();
        text_block.content = content.to_string();
        user_message.add_block(MessageBlock::PlainText(text_block));
        user_message.finalized = true;

        self.finalized_messages.push(user_message);
        self.pending_user_message = None; // Clear pending message when it becomes finalized
        Ok(())
    }

    /// Add an instruction/informational message as a finalized message
    /// This is for system messages, welcome text, etc.
    pub fn add_instruction_message(&mut self, content: &str) -> Result<()> {
        let mut instruction_message = LiveMessage::new();
        let mut text_block = PlainTextBlock::new();
        text_block.content = content.to_string();
        instruction_message.add_block(MessageBlock::PlainText(text_block));
        instruction_message.finalized = true;

        self.finalized_messages.push(instruction_message);
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

        let mut status_entries: Vec<StatusEntry> = Vec::new();
        if let Some(plan_text) = self.build_plan_text() {
            status_entries.push(StatusEntry {
                kind: StatusKind::Plan,
                content: plan_text,
                height: 0,
            });
        }

        if let Some(ref info_msg) = self.info_message {
            status_entries.push(StatusEntry {
                kind: StatusKind::Info,
                content: info_msg.clone(),
                height: 0,
            });
        } else if let Some(ref pending_msg) = self.pending_user_message {
            status_entries.push(StatusEntry {
                kind: StatusKind::Pending,
                content: pending_msg.clone(),
                height: 0,
            });
        }

        let mut status_height: u16 = 0;
        let mut error_display: Option<String> = None;

        if let Some(ref error_msg) = self.current_error {
            let formatted = Self::format_error_message(error_msg);
            let max_height = cursor_y.min(scratch_height).max(1);
            let rendered_height = Self::measure_markdown_height(&formatted, width, max_height);
            let actual_height = rendered_height.min(cursor_y);
            if actual_height > 0 {
                cursor_y = cursor_y.saturating_sub(actual_height);
                status_height = status_height.saturating_add(actual_height);
                if cursor_y > 0 {
                    cursor_y = cursor_y.saturating_sub(1);
                    status_height = status_height.saturating_add(1);
                }
            }
            error_display = Some(formatted);
        } else if !status_entries.is_empty() {
            let mut any_rendered = false;
            for idx in 0..status_entries.len() {
                if cursor_y == 0 {
                    break;
                }

                let entry = &mut status_entries[idx];
                let max_height = cursor_y.min(scratch_height).max(1);
                let rendered_height =
                    Self::measure_markdown_height(&entry.content, width, max_height);
                let actual_height = rendered_height.min(cursor_y);
                entry.height = actual_height;

                if actual_height > 0 {
                    any_rendered = true;
                    cursor_y = cursor_y.saturating_sub(actual_height);
                    status_height = status_height.saturating_add(actual_height);

                    if idx + 1 < status_entries.len() && cursor_y > 0 {
                        cursor_y = cursor_y.saturating_sub(1);
                        status_height = status_height.saturating_add(1);
                    }
                }
            }

            if any_rendered && cursor_y > 0 {
                cursor_y = cursor_y.saturating_sub(1);
                status_height = status_height.saturating_add(1);
            }
        }

        let status_height = status_height;

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

            let [content_area, status_area, input_area] = Layout::vertical([
                Constraint::Min(1),
                Constraint::Length(status_height),
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

            // Render status area (error takes priority over other messages)
            if let Some(ref error_msg) = error_display {
                Self::render_error_message(f, status_area, error_msg);
            } else if status_entries.iter().any(|entry| entry.height > 0) {
                Self::render_status_entries(f, status_area, &status_entries);
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

            let block_height = block.calculate_height(width).min(*cursor_y);

            if block_height > 0 {
                let area = Rect::new(
                    0,
                    cursor_y.saturating_sub(block_height),
                    width,
                    block_height,
                );
                block.clone().render(area, scratch);
                *cursor_y = cursor_y.saturating_sub(block_height);

                // Add one line gap between blocks within a message
                *cursor_y = cursor_y.saturating_sub(1);
            }
        }
    }

    fn measure_markdown_height(content: &str, width: u16, max_height: u16) -> u16 {
        if content.trim().is_empty() || width == 0 || max_height == 0 {
            return 0;
        }

        let text = md::from_str(content);
        let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
        Self::measure_paragraph_height(&paragraph, width, max_height)
    }

    fn measure_paragraph_height(paragraph: &Paragraph, width: u16, max_height: u16) -> u16 {
        if width == 0 || max_height == 0 {
            return 0;
        }

        let mut tmp = Buffer::empty(Rect::new(0, 0, width, max_height));
        paragraph.render(Rect::new(0, 0, width, max_height), &mut tmp);

        let mut used = 0u16;
        for y in (0..max_height).rev() {
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
                break;
            }
        }
        used
    }

    fn build_plan_text(&self) -> Option<String> {
        let plan_state = match &self.plan_state {
            Some(plan) if !plan.entries.is_empty() => plan,
            _ => return None,
        };

        if self.plan_expanded {
            let total = plan_state.entries.len();
            let mut start = 0usize;

            if total > 4 {
                while start < total
                    && matches!(plan_state.entries[start].status, PlanItemStatus::Completed)
                    && total - start > 4
                {
                    start += 1;
                }
            }

            let end = (start + 4).min(total);
            let visible = &plan_state.entries[start..end];
            let hidden = total.saturating_sub(visible.len());

            let mut text = String::from("Plan");
            if hidden > 0 {
                text.push_str(&format!(" (+{hidden} hidden)"));
            }

            for entry in visible {
                text.push('\n');
                let marker = match entry.status {
                    PlanItemStatus::Pending => "[ ]",
                    PlanItemStatus::InProgress => "[~]",
                    PlanItemStatus::Completed => "[x]",
                };
                text.push_str(marker);
                text.push(' ');
                text.push_str(&entry.content);
            }

            Some(text)
        } else {
            let total = plan_state.entries.len();
            if let Some((index, item)) = plan_state
                .entries
                .iter()
                .enumerate()
                .find(|(_, entry)| !matches!(entry.status, PlanItemStatus::Completed))
            {
                Some(format!(
                    "Plan: {} ({} of {})",
                    item.content,
                    index + 1,
                    total
                ))
            } else {
                Some(format!("Plan: All tasks completed ({total} items)"))
            }
        }
    }

    fn render_status_entries(f: &mut Frame, area: Rect, entries: &[StatusEntry]) {
        if area.height == 0 {
            return;
        }

        let mut y = area.y;
        for (idx, entry) in entries.iter().enumerate() {
            if y >= area.y + area.height {
                break;
            }

            let remaining = area.y + area.height - y;
            if remaining == 0 {
                break;
            }

            let height = entry.height.min(remaining);
            if height == 0 {
                continue;
            }

            let entry_area = Rect::new(area.x, y, area.width, height);
            match entry.kind {
                StatusKind::Info => Self::render_info_message(f, entry_area, &entry.content),
                StatusKind::Plan => Self::render_plan_message(f, entry_area, &entry.content),
                StatusKind::Pending => Self::render_pending_message(f, entry_area, &entry.content),
            }

            y = y.saturating_add(height);
            if idx + 1 < entries.len() && y < area.y + area.height {
                Self::clear_status_gap(f, Rect::new(area.x, y, area.width, 1));
                y = y.saturating_add(1);
            }
        }
    }

    fn render_info_message(f: &mut Frame, area: Rect, message: &str) {
        if area.height == 0 {
            return;
        }

        let text = md::from_str(message);
        let paragraph = Paragraph::new(text)
            .style(Style::default().fg(Color::Gray))
            .wrap(Wrap { trim: false });

        f.render_widget(paragraph, area);
    }

    fn render_plan_message(f: &mut Frame, area: Rect, plan_text: &str) {
        if area.height == 0 {
            return;
        }

        let text = md::from_str(plan_text);
        let paragraph = Paragraph::new(text)
            .style(Style::default().fg(Color::Gray).add_modifier(Modifier::DIM))
            .wrap(Wrap { trim: false });

        f.render_widget(paragraph, area);
    }

    fn clear_status_gap(f: &mut Frame, area: Rect) {
        if area.height == 0 {
            return;
        }

        let buffer = f.buffer_mut();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if let Some(cell) = buffer.cell_mut((x, y)) {
                    *cell = ratatui::buffer::Cell::default();
                }
            }
        }
    }

    /// Calculate the height needed for the input area based on textarea content
    pub fn calculate_input_height(&self, textarea: &TextArea) -> u16 {
        let lines = textarea.lines().len() as u16;
        // Add 1 for border, then clamp to reasonable bounds
        let height_with_border = lines + 1;
        height_with_border.clamp(2, self.max_input_rows + 1)
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

    /// Render error message with red styling and dismiss instructions
    fn render_error_message(f: &mut Frame, area: Rect, message: &str) {
        if area.height == 0 {
            return;
        }

        let error_text = Self::format_error_message(message);
        let text = md::from_str(&error_text);
        let paragraph = Paragraph::new(text)
            .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
            .wrap(Wrap { trim: false });

        f.render_widget(paragraph, area);
    }

    fn format_error_message(message: &str) -> String {
        format!("Error: {message} (Press Esc to dismiss)")
    }

    /// Set an error message to display
    pub fn set_error(&mut self, error_message: String) {
        self.current_error = Some(error_message);
    }

    /// Clear the current error message
    pub fn clear_error(&mut self) {
        self.current_error = None;
    }

    /// Check if there's currently an error being displayed
    pub fn has_error(&self) -> bool {
        self.current_error.is_some()
    }

    /// Set an info message to display
    pub fn set_info(&mut self, info_message: String) {
        self.info_message = Some(info_message);
    }

    /// Clear the current info message
    pub fn clear_info(&mut self) {
        self.info_message = None;
    }
}

impl ProductionTerminalRenderer {
    pub fn new() -> Result<Self> {
        Self::with_factory(|| {
            let (_w, h) = ratatui::crossterm::terminal::size()?;
            let backend = CrosstermBackend::new(io::stdout());
            let options = TerminalOptions {
                viewport: Viewport::Inline(h),
            };
            Terminal::with_options(backend, options).map_err(Into::into)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PlanItem, PlanItemStatus, PlanState};
    use crate::ui::terminal::message::{LiveMessage, MessageBlock, PlainTextBlock};
    use ratatui::backend::TestBackend;

    /// Create a test renderer using TestBackend for proper testing
    fn create_test_renderer(width: u16, height: u16) -> TerminalRenderer<TestBackend> {
        TerminalRenderer::with_factory(move || {
            let backend = TestBackend::new(width, height);
            let options = TerminalOptions {
                viewport: Viewport::Inline(height),
            };
            Terminal::with_options(backend, options).map_err(Into::into)
        })
        .unwrap()
    }

    /// Create a default test renderer with reasonable dimensions
    fn create_default_test_renderer() -> TerminalRenderer<TestBackend> {
        create_test_renderer(80, 20)
    }

    /// Helper to create a simple text message
    fn create_text_message(content: &str) -> LiveMessage {
        let mut message = LiveMessage::new();
        let mut text_block = PlainTextBlock::new();
        text_block.content = content.to_string();
        message.add_block(MessageBlock::PlainText(text_block));
        message.finalized = true;
        message
    }

    mod scrollback_tests {
        use super::*;

        #[test]
        fn test_basic_renderer_creation_and_state() {
            let renderer = create_default_test_renderer();
            assert_eq!(renderer.finalized_messages.len(), 0);
            assert!(renderer.live_message.is_none());
            assert_eq!(renderer.last_overflow, 0);
            assert!(!renderer.has_error());
        }

        #[test]
        fn test_message_finalization_workflow() {
            let mut renderer = create_default_test_renderer();

            // Start a new message
            renderer.start_new_message(1);
            assert!(renderer.live_message.is_some());
            assert_eq!(renderer.finalized_messages.len(), 0);

            // Add content to live message
            renderer.ensure_last_block_type(MessageBlock::PlainText(PlainTextBlock::new()));
            renderer.append_to_live_block("Test content");

            // Verify live message has content
            let live_message = renderer.live_message.as_ref().unwrap();
            assert!(live_message.has_content());
            assert!(!live_message.finalized);

            // Start another message - should finalize the previous one
            renderer.start_new_message(2);

            // Previous message should be finalized
            assert_eq!(renderer.finalized_messages.len(), 1);
            assert!(renderer.finalized_messages[0].finalized);
            assert!(renderer.finalized_messages[0].has_content());

            // New live message should be empty
            let new_live = renderer.live_message.as_ref().unwrap();
            assert!(!new_live.has_content());
            assert!(!new_live.finalized);
        }

        #[test]
        fn test_ensure_last_block_type_behavior() {
            let mut renderer = create_default_test_renderer();

            // Start a message
            renderer.start_new_message(1);

            // First call should create a new block
            let created_new =
                renderer.ensure_last_block_type(MessageBlock::PlainText(PlainTextBlock::new()));
            assert!(created_new, "Should create new block when none exists");

            // Second call with same type should not create new block
            let created_new =
                renderer.ensure_last_block_type(MessageBlock::PlainText(PlainTextBlock::new()));
            assert!(
                !created_new,
                "Should not create new block when same type exists"
            );

            // Call with different type should create new block
            let created_new = renderer.ensure_last_block_type(MessageBlock::Thinking(
                crate::ui::terminal::message::ThinkingBlock::new(),
            ));
            assert!(
                created_new,
                "Should create new block when different type requested"
            );

            // Verify we have 2 blocks now
            let live_message = renderer.live_message.as_ref().unwrap();
            assert_eq!(live_message.blocks.len(), 2);
        }

        #[test]
        fn test_content_appending_to_blocks() {
            let mut renderer = create_default_test_renderer();

            // Start a message
            renderer.start_new_message(1);

            // Add a text block and append content
            renderer.ensure_last_block_type(MessageBlock::PlainText(PlainTextBlock::new()));
            renderer.append_to_live_block("Hello ");
            renderer.append_to_live_block("world!");

            // Verify content was appended
            let live_message = renderer.live_message.as_ref().unwrap();
            assert_eq!(live_message.blocks.len(), 1);

            if let MessageBlock::PlainText(text_block) = &live_message.blocks[0] {
                assert_eq!(text_block.content, "Hello world!");
            } else {
                panic!("Expected PlainText block");
            }
        }

        #[test]
        fn test_pending_message_rendering() {
            let mut renderer = create_default_test_renderer();
            let textarea = tui_textarea::TextArea::default();

            // Initially no pending message - should render only input area
            renderer.render(&textarea).unwrap();
            let buffer = renderer.terminal.backend().buffer();

            // Check that most of the screen is empty (no pending message content)
            let mut has_content_above_input = false;
            for y in 0..15 {
                // Check above input area
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    if !cell.symbol().trim().is_empty() {
                        has_content_above_input = true;
                        break;
                    }
                }
                if has_content_above_input {
                    break;
                }
            }
            assert!(
                !has_content_above_input,
                "Should have no content above input when no pending message"
            );

            // Set pending message and render
            renderer.set_pending_user_message(Some("User is typing a message...".to_string()));
            renderer.render(&textarea).unwrap();
            let buffer = renderer.terminal.backend().buffer();

            // Verify pending message is rendered in dimmed style above input
            let mut found_pending_text = false;
            for y in 0..18 {
                // Check in status area above input
                let mut line_text = String::new();
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    line_text.push_str(cell.symbol());
                }
                if line_text.contains("User is typing") {
                    found_pending_text = true;
                    break;
                }
            }
            assert!(found_pending_text, "Should render pending message text");

            // Clear pending message and verify it's gone
            renderer.set_pending_user_message(None);
            renderer.render(&textarea).unwrap();
            let buffer = renderer.terminal.backend().buffer();

            let mut found_pending_after_clear = false;
            for y in 0..18 {
                let mut line_text = String::new();
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    line_text.push_str(cell.symbol());
                }
                if line_text.contains("User is typing") {
                    found_pending_after_clear = true;
                    break;
                }
            }
            assert!(
                !found_pending_after_clear,
                "Pending message should be cleared from rendering"
            );
        }

        #[test]
        fn test_plan_collapsed_summary_rendering() {
            let mut renderer = create_default_test_renderer();
            let textarea = tui_textarea::TextArea::default();

            let plan_state = PlanState {
                entries: vec![
                    PlanItem {
                        content: "Gather requirements".to_string(),
                        status: PlanItemStatus::Completed,
                        ..Default::default()
                    },
                    PlanItem {
                        content: "Update documentation".to_string(),
                        ..Default::default()
                    },
                    PlanItem {
                        content: "Review changes".to_string(),
                        ..Default::default()
                    },
                    PlanItem {
                        content: "Publish release".to_string(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            };

            renderer.set_plan_state(Some(plan_state));
            renderer.set_plan_expanded(false);

            renderer.render(&textarea).unwrap();
            let buffer = renderer.terminal.backend().buffer();

            let mut found_summary = false;
            for y in 0..18 {
                let mut line_text = String::new();
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    line_text.push_str(cell.symbol());
                }
                if line_text.contains("Plan: Update documentation (2 of 4)") {
                    found_summary = true;
                    break;
                }
            }

            assert!(found_summary, "Collapsed plan summary should be rendered");
        }

        #[test]
        fn test_plan_expanded_rendering_limits_entries() {
            let mut renderer = create_default_test_renderer();
            renderer.set_plan_expanded(true);
            let textarea = tui_textarea::TextArea::default();

            let plan_state = PlanState {
                entries: vec![
                    PlanItem {
                        content: "Draft summary".to_string(),
                        status: PlanItemStatus::Completed,
                        ..Default::default()
                    },
                    PlanItem {
                        content: "Backfill tests".to_string(),
                        status: PlanItemStatus::Completed,
                        ..Default::default()
                    },
                    PlanItem {
                        content: "Update docs".to_string(),
                        ..Default::default()
                    },
                    PlanItem {
                        content: "Refactor module".to_string(),
                        status: PlanItemStatus::InProgress,
                        ..Default::default()
                    },
                    PlanItem {
                        content: "Polish UI".to_string(),
                        ..Default::default()
                    },
                    PlanItem {
                        content: "Celebrate".to_string(),
                        status: PlanItemStatus::Completed,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            };

            renderer.set_plan_state(Some(plan_state));

            renderer.render(&textarea).unwrap();
            let buffer = renderer.terminal.backend().buffer();

            let mut header_found = false;
            let mut plan_item_lines = 0;
            let mut found_update_docs = false;
            let mut found_refactor = false;
            let mut found_polish = false;
            let mut found_celebrate = false;
            let mut hidden_completed_present = false;

            for y in 0..18 {
                let mut line_text = String::new();
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    line_text.push_str(cell.symbol());
                }

                if line_text.contains("Plan (+2 hidden)") {
                    header_found = true;
                }

                let trimmed = line_text.trim_start();
                if trimmed.starts_with('[') {
                    plan_item_lines += 1;
                    if trimmed.contains("Update docs") {
                        found_update_docs = true;
                    }
                    if trimmed.contains("Refactor module") {
                        found_refactor = true;
                    }
                    if trimmed.contains("Polish UI") {
                        found_polish = true;
                    }
                    if trimmed.contains("Celebrate") {
                        found_celebrate = true;
                    }
                    if trimmed.contains("Draft summary") {
                        hidden_completed_present = true;
                    }
                }
            }

            assert!(
                header_found,
                "Expanded plan header should include hidden count"
            );
            assert_eq!(
                plan_item_lines, 4,
                "Expanded view should render at most four plan items"
            );
            assert!(
                found_update_docs,
                "Expanded view must include first non-completed item"
            );
            assert!(
                found_refactor,
                "Expanded view must include in-progress item"
            );
            assert!(
                found_polish,
                "Expanded view must include subsequent pending item"
            );
            assert!(
                found_celebrate,
                "Expanded view should include trailing item within limit"
            );
            assert!(
                !hidden_completed_present,
                "Completed items above the window should be skipped"
            );
        }

        #[test]
        fn test_error_message_rendering() {
            let mut renderer = create_default_test_renderer();
            let textarea = tui_textarea::TextArea::default();

            // Initially no error - should render cleanly
            renderer.render(&textarea).unwrap();
            let buffer = renderer.terminal.backend().buffer();

            // Check that no error text is present
            let mut found_error_text = false;
            for y in 0..20 {
                let mut line_text = String::new();
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    line_text.push_str(cell.symbol());
                }
                if line_text.contains("Error:") {
                    found_error_text = true;
                    break;
                }
            }
            assert!(!found_error_text, "Should have no error text initially");

            // Set error and render
            renderer.set_error("Something went wrong".to_string());
            renderer.render(&textarea).unwrap();
            let buffer = renderer.terminal.backend().buffer();

            // Verify error message is rendered with "Error:" prefix and dismiss instruction
            let mut found_error_prefix = false;
            let mut found_error_content = false;
            let mut found_dismiss_instruction = false;

            for y in 0..18 {
                // Check in status area above input
                let mut line_text = String::new();
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    line_text.push_str(cell.symbol());
                }
                if line_text.contains("Error:") {
                    found_error_prefix = true;
                }
                if line_text.contains("Something went wrong") {
                    found_error_content = true;
                }
                if line_text.contains("Press Esc to dismiss") {
                    found_dismiss_instruction = true;
                }
            }

            assert!(found_error_prefix, "Should render 'Error:' prefix");
            assert!(found_error_content, "Should render error message content");
            assert!(
                found_dismiss_instruction,
                "Should render dismiss instruction"
            );

            // Clear error and verify it's gone
            renderer.clear_error();
            renderer.render(&textarea).unwrap();
            let buffer = renderer.terminal.backend().buffer();

            let mut found_error_after_clear = false;
            for y in 0..18 {
                let mut line_text = String::new();
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    line_text.push_str(cell.symbol());
                }
                if line_text.contains("Error:") || line_text.contains("Something went wrong") {
                    found_error_after_clear = true;
                    break;
                }
            }
            assert!(
                !found_error_after_clear,
                "Error message should be cleared from rendering"
            );
        }

        #[test]
        fn test_spinner_state_management() {
            let mut renderer = create_default_test_renderer();

            // Initially hidden
            assert!(matches!(renderer.spinner_state, SpinnerState::Hidden));

            // Start new message should show loading spinner
            renderer.start_new_message(1);
            assert!(matches!(
                renderer.spinner_state,
                SpinnerState::Loading { .. }
            ));

            // Hide spinner
            renderer.hide_spinner();
            assert!(matches!(renderer.spinner_state, SpinnerState::Hidden));

            // Show rate limit spinner
            renderer.show_rate_limit_spinner(30);
            assert!(matches!(
                renderer.spinner_state,
                SpinnerState::RateLimit {
                    seconds_remaining: 30,
                    ..
                }
            ));
        }

        #[test]
        fn test_clear_all_messages() {
            let mut renderer = create_default_test_renderer();

            // Add some finalized messages
            for i in 0..3 {
                let message = create_text_message(&format!("Message {i}"));
                renderer.finalized_messages.push(message);
            }

            // Add live message
            renderer.start_new_message(1);
            renderer.ensure_last_block_type(MessageBlock::PlainText(PlainTextBlock::new()));
            renderer.append_to_live_block("Live content");

            // Set some state
            renderer.last_overflow = 10;
            renderer.show_rate_limit_spinner(30);

            // Clear all messages
            renderer.clear_all_messages();

            // Everything should be reset
            assert_eq!(renderer.last_overflow, 0);
            assert!(renderer.finalized_messages.is_empty());
            assert!(renderer.live_message.is_none());
            assert!(matches!(renderer.spinner_state, SpinnerState::Hidden));
        }

        #[test]
        fn test_tool_status_updates() {
            let mut renderer = create_default_test_renderer();

            // Start a message with a tool block
            renderer.start_new_message(1);
            renderer.start_tool_use_block("test_tool".to_string(), "tool_1".to_string());

            // Update tool status
            renderer.update_tool_status(
                "tool_1",
                crate::ui::ToolStatus::Running,
                Some("Processing...".to_string()),
                None,
            );

            // Verify tool block was updated
            let live_message = renderer.live_message.as_ref().unwrap();
            assert_eq!(live_message.blocks.len(), 1);

            if let MessageBlock::ToolUse(tool_block) = &live_message.blocks[0] {
                assert_eq!(tool_block.status, crate::ui::ToolStatus::Running);
                assert_eq!(tool_block.status_message, Some("Processing...".to_string()));
            } else {
                panic!("Expected ToolUse block");
            }
        }
    }

    mod message_height_tests {
        use super::*;
        use crate::ui::terminal::message::{ThinkingBlock, ToolUseBlock};

        #[test]
        fn test_plain_text_height_calculation() {
            let width = 80;

            // Test empty content
            let empty_block = PlainTextBlock::new();
            let message_block = MessageBlock::PlainText(empty_block);
            assert_eq!(
                message_block.calculate_height(width),
                0,
                "Empty content should have 0 height"
            );

            // Test single line
            let mut single_line_block = PlainTextBlock::new();
            single_line_block.content = "Hello world".to_string();
            let message_block = MessageBlock::PlainText(single_line_block);
            assert_eq!(
                message_block.calculate_height(width),
                1,
                "Single line should have height 1"
            );

            // Test multiple lines
            let mut multi_line_block = PlainTextBlock::new();
            multi_line_block.content = "Line 1\nLine 2\nLine 3".to_string();
            let message_block = MessageBlock::PlainText(multi_line_block);
            assert_eq!(
                message_block.calculate_height(width),
                3,
                "Three lines should have height 3"
            );

            // Test line wrapping
            let long_line = "a".repeat(160); // Should wrap to 2 lines with width 80
            let mut wrap_block = PlainTextBlock::new();
            wrap_block.content = long_line;
            let message_block = MessageBlock::PlainText(wrap_block);
            assert_eq!(
                message_block.calculate_height(width),
                2,
                "Long line should wrap to 2 lines"
            );
        }

        #[test]
        fn test_thinking_block_height_calculation() {
            let width = 80;

            // Test empty thinking block
            let empty_thinking = ThinkingBlock::new();
            let message_block = MessageBlock::Thinking(empty_thinking);
            assert_eq!(
                message_block.calculate_height(width),
                0,
                "Empty thinking block should have 0 height"
            );

            // Test thinking block with content
            let mut thinking_block = ThinkingBlock::new();
            thinking_block.content = "I'm thinking about this problem".to_string();
            let message_block = MessageBlock::Thinking(thinking_block);
            assert!(
                message_block.calculate_height(width) >= 1,
                "Thinking block with content should have height >= 1"
            );
        }

        #[test]
        fn test_tool_use_block_height_calculation() {
            let width = 80;

            // Test basic tool block
            let tool_block = ToolUseBlock::new("test_tool".to_string(), "tool_id_1".to_string());
            let message_block = MessageBlock::ToolUse(tool_block);
            assert_eq!(
                message_block.calculate_height(width),
                1,
                "Basic tool block should have height 1 (tool name line)"
            );

            // Test tool block with simple parameters
            let mut tool_block =
                ToolUseBlock::new("write_file".to_string(), "tool_id_2".to_string());
            tool_block.add_or_update_parameter("path".to_string(), "test.txt".to_string());
            tool_block.add_or_update_parameter("content".to_string(), "Hello\nWorld".to_string());

            let message_block = MessageBlock::ToolUse(tool_block);
            let height = message_block.calculate_height(width);

            // Should include: tool name + path parameter + content parameter (full-width)
            assert!(
                height > 1,
                "Tool block with parameters should have height > 1"
            );
            assert!(height < 20, "Tool block height should be reasonable"); // Sanity check

            // Test edit tool with old_text and new_text (should show combined diff)
            let mut edit_tool = ToolUseBlock::new("edit".to_string(), "edit_id".to_string());
            edit_tool.status = crate::ui::ToolStatus::Success;
            edit_tool.add_or_update_parameter("old_text".to_string(), "old content".to_string());
            edit_tool.add_or_update_parameter("new_text".to_string(), "new content".to_string());

            let message_block = MessageBlock::ToolUse(edit_tool);
            let height = message_block.calculate_height(width);

            // Should include: tool name + diff section
            assert!(height >= 2, "Edit tool with diff should have height >= 2");
        }

        #[test]
        fn test_message_block_edge_cases() {
            let width = 1; // Very narrow width

            // Test with narrow width
            let mut text_block = PlainTextBlock::new();
            text_block.content = "Hello".to_string(); // 5 chars should wrap to 5 lines
            let message_block = MessageBlock::PlainText(text_block);
            assert_eq!(
                message_block.calculate_height(width),
                5,
                "Each character should be on its own line with width 1"
            );

            // Test with zero width (edge case)
            let mut text_block = PlainTextBlock::new();
            text_block.content = "Hello".to_string();
            let message_block = MessageBlock::PlainText(text_block);
            let height = message_block.calculate_height(0);
            assert!(height > 0, "Should handle zero width gracefully");
        }
    }

    mod input_height_tests {
        use super::*;

        #[test]
        fn test_input_height_calculation() {
            let renderer = create_default_test_renderer();

            // Test empty textarea
            let textarea = tui_textarea::TextArea::default();
            let height = renderer.calculate_input_height(&textarea);
            assert_eq!(
                height, 2,
                "Empty textarea should have minimum height (1 content + 1 border)"
            );

            // Test single line content
            let mut textarea = tui_textarea::TextArea::default();
            textarea.insert_str("Hello");
            let height = renderer.calculate_input_height(&textarea);
            assert_eq!(height, 2, "Single line should still be minimum height");

            // Test multiple lines
            let mut textarea = tui_textarea::TextArea::default();
            textarea.insert_str("Line 1\nLine 2\nLine 3");
            let height = renderer.calculate_input_height(&textarea);
            assert_eq!(
                height, 4,
                "Three lines should give height 4 (3 content + 1 border)"
            );

            // Test max height constraint
            let mut textarea = tui_textarea::TextArea::default();
            let many_lines = (0..10)
                .map(|i| format!("Line {i}"))
                .collect::<Vec<_>>()
                .join("\n");
            textarea.insert_str(&many_lines);
            let height = renderer.calculate_input_height(&textarea);
            assert_eq!(
                height,
                renderer.max_input_rows + 1,
                "Should be capped at max_input_rows + border"
            );
        }

        #[test]
        fn test_input_height_constraints() {
            let renderer = create_default_test_renderer();

            // Test that height is always at least 2 (content + border)
            let textarea = tui_textarea::TextArea::default();
            let height = renderer.calculate_input_height(&textarea);
            assert!(height >= 2, "Height should always be at least 2");

            // Test that height never exceeds max_input_rows + 1
            let mut textarea = tui_textarea::TextArea::default();
            let excessive_lines = (0..100)
                .map(|i| format!("Line {i}"))
                .collect::<Vec<_>>()
                .join("\n");
            textarea.insert_str(&excessive_lines);
            let height = renderer.calculate_input_height(&textarea);
            assert!(
                height <= renderer.max_input_rows + 1,
                "Height should never exceed max_input_rows + 1"
            );
        }
    }

    mod integration_tests {
        use super::*;

        #[test]
        fn test_complete_message_workflow_rendering() {
            let mut renderer = create_default_test_renderer();
            let textarea = tui_textarea::TextArea::default();

            // 1. Start streaming - should show spinner
            renderer.start_new_message(1);
            renderer.render(&textarea).unwrap();
            let buffer = renderer.terminal.backend().buffer();

            // Look for spinner character (braille patterns)
            let mut found_spinner = false;
            for y in 0..18 {
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    let symbol = cell.symbol();
                    if symbol.chars().any(|c| {
                        matches!(c, '⠋' | '⠙' | '⠹' | '⠸' | '⠼' | '⠴' | '⠦' | '⠧' | '⠇' | '⠏')
                    }) {
                        found_spinner = true;
                        break;
                    }
                }
                if found_spinner {
                    break;
                }
            }
            assert!(
                found_spinner,
                "Should show loading spinner when streaming starts"
            );

            // 2. Add some text content - spinner should disappear
            renderer.ensure_last_block_type(MessageBlock::PlainText(PlainTextBlock::new()));
            renderer.append_to_live_block("Here's my response: ");
            renderer.render(&textarea).unwrap();
            let buffer = renderer.terminal.backend().buffer();

            // Check that text content is rendered
            let mut found_response_text = false;
            for y in 0..18 {
                let mut line_text = String::new();
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    line_text.push_str(cell.symbol());
                }
                if line_text.contains("Here's my response") {
                    found_response_text = true;
                    break;
                }
            }
            assert!(
                found_response_text,
                "Should render live message text content"
            );

            // 3. Start a tool - should render tool block
            renderer.start_tool_use_block("write_file".to_string(), "tool_1".to_string());
            renderer.add_or_update_tool_parameter(
                "tool_1",
                "path".to_string(),
                "test.txt".to_string(),
            );
            renderer.render(&textarea).unwrap();
            let buffer = renderer.terminal.backend().buffer();

            let mut found_tool_name = false;
            let mut found_path_param = false;
            for y in 0..18 {
                let mut line_text = String::new();
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    line_text.push_str(cell.symbol());
                }
                if line_text.contains("write_file") {
                    found_tool_name = true;
                }
                if line_text.contains("test.txt") {
                    found_path_param = true;
                }
            }
            assert!(found_tool_name, "Should render tool name");
            assert!(found_path_param, "Should render tool parameters");

            // 4. Update tool status - should reflect in rendering (status indicator, not output)
            renderer.update_tool_status(
                "tool_1",
                crate::ui::ToolStatus::Success,
                None,
                Some("File written successfully".to_string()), // This is for LLM, not UI
            );
            renderer.render(&textarea).unwrap();
            let buffer = renderer.terminal.backend().buffer();

            // Look for tool with success status indicator (green bullet)
            let mut found_success_status = false;
            for y in 0..18 {
                let mut line_text = String::new();
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    line_text.push_str(cell.symbol());
                }
                // Look for the tool name line
                if line_text.contains("write_file") {
                    // Check if the status symbol (first character) is green (success)
                    let status_cell = buffer.cell((0, y)).unwrap();
                    if status_cell.fg == Color::Green && status_cell.symbol() == "●" {
                        found_success_status = true;
                        break;
                    }
                }
            }
            assert!(
                found_success_status,
                "Should render tool with green success status indicator"
            );

            // 5. Finalize message by starting new one
            renderer.start_new_message(2);
            assert_eq!(renderer.finalized_messages.len(), 1);
            assert!(renderer.finalized_messages[0].finalized);
        }

        #[test]
        fn test_scrollback_behavior_with_overflow() {
            // Create a smaller terminal to test scrollback more easily
            let mut renderer = create_test_renderer(80, 10); // Only 10 lines tall (8 content + 2 input)
            let textarea = tui_textarea::TextArea::default();

            // Add exactly 3 finalized messages with predictable single-line content
            for i in 0..3 {
                let message = create_text_message(&format!("Message {i}"));
                renderer.finalized_messages.push(message);
            }

            // Add a live message that will push content into scrollback
            renderer.start_new_message(1);
            renderer.ensure_last_block_type(MessageBlock::PlainText(PlainTextBlock::new()));
            renderer.append_to_live_block("Live message content");

            // First render - should have minimal overflow
            renderer.render(&textarea).unwrap();
            let backend = renderer.terminal.backend();

            // Capture what's in viewport and scrollback
            let buffer = backend.buffer();
            let scrollback = backend.scrollback();

            // Extract visible content (excluding input area at bottom)
            let mut viewport_lines = Vec::new();
            for y in 0..8 {
                // 8 lines for content (10 total - 2 for input)
                let mut line_text = String::new();
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    line_text.push_str(cell.symbol());
                }
                let trimmed = line_text.trim_end();
                if !trimmed.is_empty() {
                    viewport_lines.push(format!("V{y}: {trimmed}"));
                }
            }

            // Extract scrollback content
            let mut scrollback_lines = Vec::new();
            for y in 0..scrollback.area().height {
                let mut line_text = String::new();
                for x in 0..scrollback.area().width {
                    if let Some(cell) = scrollback.cell((x, y)) {
                        line_text.push_str(cell.symbol());
                    }
                }
                let trimmed = line_text.trim_end();
                if !trimmed.is_empty() {
                    scrollback_lines.push(format!("S{y}: {trimmed}"));
                }
            }

            println!("=== After first render ===");
            println!("Viewport content:");
            for line in &viewport_lines {
                println!("  {line}");
            }
            println!("Scrollback content:");
            for line in &scrollback_lines {
                println!("  {line}");
            }
            println!("last_overflow: {}", renderer.last_overflow);

            // Now add more content to force more overflow
            for i in 3..6 {
                let message = create_text_message(&format!("Additional message {i}"));
                renderer.finalized_messages.push(message);
            }

            // Second render - should push more content to scrollback
            renderer.render(&textarea).unwrap();
            let backend = renderer.terminal.backend();

            // Capture content again
            let buffer = backend.buffer();
            let scrollback = backend.scrollback();

            let mut viewport_lines_2 = Vec::new();
            for y in 0..8 {
                let mut line_text = String::new();
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    line_text.push_str(cell.symbol());
                }
                let trimmed = line_text.trim_end();
                if !trimmed.is_empty() {
                    viewport_lines_2.push(format!("V{y}: {trimmed}"));
                }
            }

            let mut scrollback_lines_2 = Vec::new();
            for y in 0..scrollback.area().height {
                let mut line_text = String::new();
                for x in 0..scrollback.area().width {
                    if let Some(cell) = scrollback.cell((x, y)) {
                        line_text.push_str(cell.symbol());
                    }
                }
                let trimmed = line_text.trim_end();
                if !trimmed.is_empty() {
                    scrollback_lines_2.push(format!("S{y}: {trimmed}"));
                }
            }

            println!("=== After second render ===");
            println!("Viewport content:");
            for line in &viewport_lines_2 {
                println!("  {line}");
            }
            println!("Scrollback content:");
            for line in &scrollback_lines_2 {
                println!("  {line}");
            }
            println!("last_overflow: {}", renderer.last_overflow);

            // Specific assertions to catch duplication bugs:

            // 1. Live message should be visible in viewport (closest to input)
            let viewport_text = viewport_lines_2.join(" ");
            assert!(
                viewport_text.contains("Live message content"),
                "Live message should be visible in viewport"
            );

            // 2. Check for duplication - no message should appear in both viewport and scrollback

            // Extract unique message identifiers from both areas
            let mut viewport_messages = std::collections::HashSet::new();
            let mut scrollback_messages = std::collections::HashSet::new();

            // Simple regex-free approach: look for exact patterns
            for line in &viewport_lines_2 {
                // Look for "Message X" patterns
                if let Some(start) = line.find("Message ") {
                    let after_msg = &line[start + 8..]; // "Message ".len() = 8
                    if let Some(space_pos) = after_msg.find(' ') {
                        let id = &after_msg[..space_pos];
                        viewport_messages.insert(format!("Message {id}"));
                    } else if !after_msg.is_empty() {
                        viewport_messages.insert(format!("Message {after_msg}"));
                    }
                }
                // Look for "Additional message X" patterns
                if let Some(start) = line.find("Additional message ") {
                    let after_msg = &line[start + 19..]; // "Additional message ".len() = 19
                    if let Some(space_pos) = after_msg.find(' ') {
                        let id = &after_msg[..space_pos];
                        viewport_messages.insert(format!("Additional message {id}"));
                    } else if !after_msg.is_empty() {
                        viewport_messages.insert(format!("Additional message {after_msg}"));
                    }
                }
            }

            for line in &scrollback_lines_2 {
                // Look for "Message X" patterns
                if let Some(start) = line.find("Message ") {
                    let after_msg = &line[start + 8..];
                    if let Some(space_pos) = after_msg.find(' ') {
                        let id = &after_msg[..space_pos];
                        scrollback_messages.insert(format!("Message {id}"));
                    } else if !after_msg.is_empty() {
                        scrollback_messages.insert(format!("Message {after_msg}"));
                    }
                }
                // Look for "Additional message X" patterns
                if let Some(start) = line.find("Additional message ") {
                    let after_msg = &line[start + 19..];
                    if let Some(space_pos) = after_msg.find(' ') {
                        let id = &after_msg[..space_pos];
                        scrollback_messages.insert(format!("Additional message {id}"));
                    } else if !after_msg.is_empty() {
                        scrollback_messages.insert(format!("Additional message {after_msg}"));
                    }
                }
            }

            // 3. No message should appear in both viewport and scrollback
            let intersection: Vec<_> = viewport_messages
                .intersection(&scrollback_messages)
                .collect();
            assert!(
                intersection.is_empty(),
                "Messages should not be duplicated between viewport and scrollback: {intersection:?}"
            );

            // 4. Scrollback should contain older content
            assert!(
                !scrollback_lines_2.is_empty(),
                "Should have content in scrollback"
            );

            // 5. Verify overflow tracking is reasonable
            assert!(renderer.last_overflow > 0, "Should track overflow amount");

            // 6. Total unique messages should equal what we added
            let total_unique_messages = viewport_messages.len() + scrollback_messages.len();
            assert!(
                total_unique_messages <= 6,
                "Should not have more messages than we created"
            );
            assert!(
                total_unique_messages >= 3,
                "Should have at least some of our messages visible"
            );
        }

        #[test]
        fn test_tall_live_message_modification_no_duplication() {
            // Test the scenario where a live message is already tall and being modified
            // This should not cause duplication in scrollback
            let mut renderer = create_test_renderer(40, 8); // Very small terminal: 6 content + 2 input
            let textarea = tui_textarea::TextArea::default();

            // Add one finalized message first
            let message = create_text_message("Previous message");
            renderer.finalized_messages.push(message);

            // Start a live message with a tool that will be tall
            renderer.start_new_message(1);
            renderer.start_tool_use_block("write_file".to_string(), "tool_1".to_string());
            renderer.add_or_update_tool_parameter(
                "tool_1",
                "path".to_string(),
                "long_file.txt".to_string(),
            );

            // Add a large content parameter that will make the tool block tall
            let large_content = (0..10)
                .map(|i| format!("Line {i} of file content"))
                .collect::<Vec<_>>()
                .join("\n");
            renderer.add_or_update_tool_parameter("tool_1", "content".to_string(), large_content);

            // First render - live message should already overflow
            renderer.render(&textarea).unwrap();
            let backend = renderer.terminal.backend();

            let mut initial_scrollback_lines = Vec::new();
            let scrollback = backend.scrollback();
            for y in 0..scrollback.area().height {
                let mut line_text = String::new();
                for x in 0..scrollback.area().width {
                    if let Some(cell) = scrollback.cell((x, y)) {
                        line_text.push_str(cell.symbol());
                    }
                }
                let trimmed = line_text.trim_end();
                if !trimmed.is_empty() {
                    initial_scrollback_lines.push(trimmed.to_string());
                }
            }

            println!("=== Initial scrollback after tall live message ===");
            for (i, line) in initial_scrollback_lines.iter().enumerate() {
                println!("  {i}: {line}");
            }

            // Now modify the live message by updating tool status
            renderer.update_tool_status("tool_1", crate::ui::ToolStatus::Success, None, None);

            // Second render - should not duplicate content in scrollback
            renderer.render(&textarea).unwrap();
            let backend = renderer.terminal.backend();

            let mut modified_scrollback_lines = Vec::new();
            let scrollback = backend.scrollback();
            for y in 0..scrollback.area().height {
                let mut line_text = String::new();
                for x in 0..scrollback.area().width {
                    if let Some(cell) = scrollback.cell((x, y)) {
                        line_text.push_str(cell.symbol());
                    }
                }
                let trimmed = line_text.trim_end();
                if !trimmed.is_empty() {
                    modified_scrollback_lines.push(trimmed.to_string());
                }
            }

            println!("=== Scrollback after live message modification ===");
            for (i, line) in modified_scrollback_lines.iter().enumerate() {
                println!("  {i}: {line}");
            }

            // Key assertion: scrollback should not have grown significantly
            // It might grow slightly due to status change, but should not double
            let initial_non_empty_lines = initial_scrollback_lines.len();
            let modified_non_empty_lines = modified_scrollback_lines.len();

            assert!(
                modified_non_empty_lines <= initial_non_empty_lines + 2,
                "Scrollback should not grow significantly when modifying live message.\
                Initial: {initial_non_empty_lines}, Modified: {modified_non_empty_lines}"
            );

            // Check for obvious duplication patterns
            let mut line_counts = std::collections::HashMap::new();
            for line in &modified_scrollback_lines {
                *line_counts.entry(line.clone()).or_insert(0) += 1;
            }

            let duplicated_lines: Vec<_> =
                line_counts.iter().filter(|(_, &count)| count > 1).collect();

            assert!(
                duplicated_lines.is_empty(),
                "Found duplicated lines in scrollback: {duplicated_lines:?}",
            );

            // Add more content to the live message
            renderer.append_to_live_block("\n\nAdditional text appended to live message");

            // Third render
            renderer.render(&textarea).unwrap();
            let backend = renderer.terminal.backend();

            let mut final_scrollback_lines = Vec::new();
            let scrollback = backend.scrollback();
            for y in 0..scrollback.area().height {
                let mut line_text = String::new();
                for x in 0..scrollback.area().width {
                    if let Some(cell) = scrollback.cell((x, y)) {
                        line_text.push_str(cell.symbol());
                    }
                }
                let trimmed = line_text.trim_end();
                if !trimmed.is_empty() {
                    final_scrollback_lines.push(trimmed.to_string());
                }
            }

            println!("=== Final scrollback after appending to live message ===");
            for (i, line) in final_scrollback_lines.iter().enumerate() {
                println!("  {i}: {line}");
            }

            // Final assertion: should not have excessive duplication
            let mut final_line_counts = std::collections::HashMap::new();
            for line in &final_scrollback_lines {
                *final_line_counts.entry(line.clone()).or_insert(0) += 1;
            }

            let final_duplicated_lines: Vec<_> = final_line_counts
                .iter()
                .filter(|(_, &count)| count > 1)
                .collect();

            assert!(
                final_duplicated_lines.is_empty(),
                "Found duplicated lines in final scrollback: {final_duplicated_lines:?}"
            );
        }

        #[test]
        fn test_live_message_rendering_priority() {
            let mut renderer = create_default_test_renderer();
            let textarea = tui_textarea::TextArea::default();

            // Add some finalized messages
            for i in 0..2 {
                let message = create_text_message(&format!("Finalized message {i}"));
                renderer.finalized_messages.push(message);
            }

            // Start a live message
            renderer.start_new_message(1);
            renderer.ensure_last_block_type(MessageBlock::PlainText(PlainTextBlock::new()));
            renderer.append_to_live_block("This is live content being streamed");

            renderer.render(&textarea).unwrap();
            let buffer = renderer.terminal.backend().buffer();

            // Live content should appear closest to input (bottom of content area)
            let mut found_live_content = false;
            let mut found_finalized_content = false;
            let mut live_content_y = None;
            let mut finalized_content_y = None;

            for y in 0..18 {
                let mut line_text = String::new();
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    line_text.push_str(cell.symbol());
                }

                if line_text.contains("live content being streamed") {
                    found_live_content = true;
                    live_content_y = Some(y);
                }
                if line_text.contains("Finalized message") {
                    found_finalized_content = true;
                    if finalized_content_y.is_none() {
                        finalized_content_y = Some(y);
                    }
                }
            }

            assert!(found_live_content, "Should render live content");
            assert!(found_finalized_content, "Should render finalized content");

            // Live content should appear below (higher y coordinate) finalized content
            if let (Some(live_y), Some(finalized_y)) = (live_content_y, finalized_content_y) {
                assert!(
                    live_y > finalized_y,
                    "Live content should appear closer to input than finalized content"
                );
            }
        }

        #[test]
        fn test_spinner_rendering_states() {
            let mut renderer = create_default_test_renderer();
            let textarea = tui_textarea::TextArea::default();

            // Test loading spinner
            renderer.start_new_message(1);
            renderer.render(&textarea).unwrap();
            let buffer = renderer.terminal.backend().buffer();

            let mut found_loading_spinner = false;
            for y in 0..18 {
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    let symbol = cell.symbol();
                    if symbol.chars().any(|c| {
                        matches!(c, '⠋' | '⠙' | '⠹' | '⠸' | '⠼' | '⠴' | '⠦' | '⠧' | '⠇' | '⠏')
                    }) {
                        found_loading_spinner = true;
                        break;
                    }
                }
                if found_loading_spinner {
                    break;
                }
            }
            assert!(found_loading_spinner, "Should show loading spinner");

            // Test rate limit spinner with text
            renderer.show_rate_limit_spinner(30);
            renderer.render(&textarea).unwrap();
            let buffer = renderer.terminal.backend().buffer();

            let mut found_rate_limit_text = false;
            for y in 0..18 {
                let mut line_text = String::new();
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    line_text.push_str(cell.symbol());
                }
                if line_text.contains("Rate limited") && line_text.contains("30s") {
                    found_rate_limit_text = true;
                    break;
                }
            }
            assert!(
                found_rate_limit_text,
                "Should show rate limit text with countdown"
            );

            // Test hidden spinner
            renderer.hide_spinner();
            renderer.render(&textarea).unwrap();
            let buffer = renderer.terminal.backend().buffer();

            let mut found_spinner_after_hide = false;
            for y in 0..18 {
                let mut line_text = String::new();
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    line_text.push_str(cell.symbol());
                    if cell.symbol().chars().any(|c| {
                        matches!(c, '⠋' | '⠙' | '⠹' | '⠸' | '⠼' | '⠴' | '⠦' | '⠧' | '⠇' | '⠏')
                    }) {
                        found_spinner_after_hide = true;
                        break;
                    }
                }
                if line_text.contains("Rate limited") {
                    found_spinner_after_hide = true;
                    break;
                }
            }
            assert!(
                !found_spinner_after_hide,
                "Should hide spinner and rate limit text"
            );
        }

        #[test]
        fn test_error_takes_priority_over_pending_message_in_rendering() {
            let mut renderer = create_default_test_renderer();
            let textarea = tui_textarea::TextArea::default();

            // Set both pending message and error
            renderer.set_pending_user_message(Some("User is typing...".to_string()));
            renderer.set_error("Critical error occurred".to_string());

            // Render and verify error takes priority over pending message
            renderer.render(&textarea).unwrap();
            let buffer = renderer.terminal.backend().buffer();

            let mut found_error = false;
            let mut found_pending = false;

            for y in 0..18 {
                // Check status area above input
                let mut line_text = String::new();
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    line_text.push_str(cell.symbol());
                }
                if line_text.contains("Critical error occurred") || line_text.contains("Error:") {
                    found_error = true;
                }
                if line_text.contains("User is typing") {
                    found_pending = true;
                }
            }

            assert!(found_error, "Error message should be visible");
            assert!(
                !found_pending,
                "Pending message should be hidden when error is present"
            );

            // Clear error - pending message should now be visible
            renderer.clear_error();
            renderer.render(&textarea).unwrap();
            let buffer = renderer.terminal.backend().buffer();

            let mut found_error_after_clear = false;
            let mut found_pending_after_clear = false;

            for y in 0..18 {
                let mut line_text = String::new();
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    line_text.push_str(cell.symbol());
                }
                if line_text.contains("Critical error occurred") || line_text.contains("Error:") {
                    found_error_after_clear = true;
                }
                if line_text.contains("User is typing") {
                    found_pending_after_clear = true;
                }
            }

            assert!(!found_error_after_clear, "Error should be cleared");
            assert!(
                found_pending_after_clear,
                "Pending message should now be visible"
            );

            // Clear pending message - should have clean status area
            renderer.set_pending_user_message(None);
            renderer.render(&textarea).unwrap();
            let buffer = renderer.terminal.backend().buffer();

            let mut has_status_content = false;
            for y in 0..17 {
                // Check status area (excluding input border)
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    if !cell.symbol().trim().is_empty() {
                        has_status_content = true;
                        break;
                    }
                }
                if has_status_content {
                    break;
                }
            }
            assert!(
                !has_status_content,
                "Status area should be clean when no error or pending message"
            );
        }
    }
}
