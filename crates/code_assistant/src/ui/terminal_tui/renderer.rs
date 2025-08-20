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

use super::blocks::{LiveBlockType, PlainTextBlock, ThinkingBlock, ToolUseBlock};

/// Handles the terminal display and rendering using ratatui
pub struct TerminalRenderer {
    pub terminal: Terminal<CrosstermBackend<io::Stdout>>,
    /// Finalized markdown blocks (as source strings)
    pub finalized_blocks: Vec<String>,
    /// Current live block being streamed
    pub live_block: Option<LiveBlockType>,
    /// Optional pending user message (displayed between input and live content while streaming)
    pending_user_message: Option<String>,
    /// Last computed overflow (how many rows have been promoted so far); used to promote only deltas
    pub last_overflow: u16,
    /// Maximum rows for input area (including 1 for content min + border)
    pub max_input_rows: u16,
}

impl TerminalRenderer {
    pub fn new() -> Result<Self> {
        let terminal = Self::create_terminal()?;

        Ok(Self {
            terminal,
            finalized_blocks: Vec::new(),
            live_block: None,
            pending_user_message: None,
            last_overflow: 0,
            max_input_rows: 5, // max input height (content lines + border line)
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

    /// Start a new plain text live block
    pub fn start_plain_text_block(&mut self) {
        self.live_block = Some(LiveBlockType::PlainText(PlainTextBlock::new()));
        self.last_overflow = 0;
    }

    /// Start a new thinking live block
    pub fn start_thinking_block(&mut self) {
        self.live_block = Some(LiveBlockType::Thinking(ThinkingBlock::new()));
        self.last_overflow = 0;
    }

    /// Start a new tool use live block
    pub fn start_tool_use_block(&mut self, name: String, id: String) {
        self.live_block = Some(LiveBlockType::ToolUse(ToolUseBlock::new(name, id)));
        self.last_overflow = 0;
    }

    /// Legacy method for backward compatibility
    pub fn start_live_block(&mut self) {
        self.start_plain_text_block();
    }

    /// Set a pending user message (displayed while streaming)
    pub fn set_pending_user_message(&mut self, message: String) {
        self.pending_user_message = Some(message);
    }

    /// Append text to the current live block
    pub fn append_to_live_block(&mut self, text: &str) {
        if let Some(ref mut block) = self.live_block {
            block.append_content(text);
        }
    }

    /// Add or update a tool parameter
    pub fn add_or_update_tool_parameter(&mut self, tool_id: &str, name: String, value: String) {
        if let Some(ref mut block) = self.live_block {
            if let Some(tool_block) = block.get_tool_mut(tool_id) {
                tool_block.add_or_update_parameter(name, value);
            }
        }
    }

    /// Update tool status
    pub fn update_tool_status(
        &mut self,
        tool_id: &str,
        status: crate::ui::ToolStatus,
        message: Option<String>,
        output: Option<String>,
    ) {
        if let Some(ref mut block) = self.live_block {
            if let Some(tool_block) = block.get_tool_mut(tool_id) {
                tool_block.status = status;
                tool_block.status_message = message;
                tool_block.output = output;
            }
        }
    }

    /// Finalize the current live block: move remaining visible content into finalized_blocks
    /// We do NOT insert into scrollback immediately; overflow logic will promote rows as needed.
    pub fn finalize_live_block(&mut self) -> Result<()> {
        if let Some(block) = self.live_block.take() {
            if block.is_tool_use() {
                // For tool use blocks, create a detailed text representation with parameters
                if let Some(tool_block) = block.as_tool_use() {
                    let status_symbol = "●"; // Always use dot
                    let status_text = match tool_block.status {
                        crate::ui::ToolStatus::Pending => "streaming", // Gray - parameters streaming
                        crate::ui::ToolStatus::Running => "running",   // Blue - tool is executing
                        crate::ui::ToolStatus::Success => "success",   // Green - successful result
                        crate::ui::ToolStatus::Error => "error",       // Red - error result
                    };
                    
                    let mut tool_text = format!("{} {} ({})\n", status_symbol, tool_block.name, status_text);
                    
                    // Add parameters with full details
                    for (name, param) in &tool_block.parameters {
                        if should_hide_parameter(&tool_block.name, name, &param.value) {
                            continue;
                        }
                        if is_full_width_parameter(&tool_block.name, name) {
                            tool_text.push_str(&format!("  {name}:\n"));
                            // Show all lines of full-width parameters in finalized view
                            for line in param.value.lines() {
                                tool_text.push_str(&format!("    {line}\n"));
                            }
                        } else {
                            tool_text.push_str(&format!("  {}: {}\n", name, param.value)); // Show full value, not truncated
                        }
                    }
                    
                    // Add status message if present
                    if let Some(ref message) = tool_block.status_message {
                        tool_text.push_str(&format!("  Status: {message}\n"));
                    }
                    
                    self.finalized_blocks.push(tool_text);
                }
            } else if let Some(markdown_content) = block.get_markdown_content() {
                if !markdown_content.trim().is_empty() {
                    self.finalized_blocks.push(markdown_content);
                }
            }
        }
        // Keep a 1-line gap: implemented implicitly by always reserving one empty line at bottom
        self.last_overflow = 0;
        Ok(())
    }

    /// Add a user message as finalized block and clear any pending user message
    pub fn add_user_message(&mut self, content: &str) -> Result<()> {
        self.finalized_blocks.push(content.to_string());
        self.pending_user_message = None; // Clear pending message when it becomes finalized
        Ok(())
    }

    /// Render the complete UI: composed finalized content + live content + 1-line gap + input
    pub fn render(&mut self, textarea: &TextArea) -> Result<()> {
        // Phase 1: compute layout and promotion outside of draw
        let term_size = self.terminal.size()?;
        let input_height = self.calculate_input_height(textarea);
        let available = term_size.height.saturating_sub(input_height);
        let width = term_size.width;

        // Compose scratch buffer: render 1 blank line (gap), then live_text, then finalized tail above
        let headroom: u16 = 200; // keep small to reduce work per frame
        let scratch_height = available.saturating_add(headroom).max(available);
        let mut scratch = Buffer::empty(Rect::new(0, 0, width, scratch_height));

        // We pack from bottom up: bottom = gap, above it pending user msg (if any), above it live, above that finalized tail
        let mut cursor_y = scratch_height; // one past last line

        // Reserve one blank line as gap above input (at the very bottom)
        cursor_y = cursor_y.saturating_sub(1);

        // Helper: render a markdown string into temp buffer to measure height (limited to remaining space)
        let measure_and_render = |md_src: &str, dst: &mut Buffer, bottom_y: &mut u16| {
            let text = md::from_str(md_src);
            let para_for_measure = Paragraph::new(text.clone()).wrap(Wrap { trim: false });
            // Only allocate tmp up to remaining space to keep it cheap
            let max_h = (*bottom_y).clamp(1, 500); // cap to 500 to avoid huge temps
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
                return 0u16;
            }
            let h = used.min(*bottom_y);
            if h == 0 {
                return 0u16;
            }
            // render into destination aligned at bottom
            let area = Rect::new(0, bottom_y.saturating_sub(h), width, h);
            let para_for_draw = Paragraph::new(text).wrap(Wrap { trim: false });
            para_for_draw.render(area, dst);
            *bottom_y = bottom_y.saturating_sub(h);
            h
        };

        // Reserve space for pending user message if present
        let pending_height = if let Some(ref pending_msg) = self.pending_user_message {
            let _ = measure_and_render(pending_msg, &mut scratch, &mut cursor_y);
            // Add a small gap above pending message
            cursor_y = cursor_y.saturating_sub(1);
            2 // approximate height including gap
        } else {
            0
        };

        // 1) Render current live block (so it is closest to the input)
        if let Some(ref live_block) = self.live_block {
            if live_block.is_tool_use() {
                // For tool use blocks, create a detailed text representation
                if let Some(tool_block) = live_block.as_tool_use() {
                    let status_symbol = "●"; // Always use dot
                    let status_text = match tool_block.status {
                        crate::ui::ToolStatus::Pending => "streaming", // Gray - parameters streaming
                        crate::ui::ToolStatus::Running => "running",   // Blue - tool is executing
                        crate::ui::ToolStatus::Success => "success",   // Green - successful result
                        crate::ui::ToolStatus::Error => "error",       // Red - error result
                    };
                    
                    let mut tool_text = format!("{} {} ({})\n", status_symbol, tool_block.name, status_text);
                    
                    // Add parameters
                    for (name, param) in &tool_block.parameters {
                        if should_hide_parameter(&tool_block.name, name, &param.value) {
                            continue;
                        }
                        if is_full_width_parameter(&tool_block.name, name) {
                            tool_text.push_str(&format!("  {name}:\n"));
                            // Show first few lines of full-width parameters for live view
                            for line in param.value.lines().take(3) {
                                tool_text.push_str(&format!("    {line}\n"));
                            }
                            if param.value.lines().count() > 3 {
                                tool_text.push_str("    ...\n");
                            }
                        } else {
                            tool_text.push_str(&format!("  {}: {}\n", name, param.get_display_value()));
                        }
                    }
                    
                    // Add status message if present
                    if let Some(ref message) = tool_block.status_message {
                        tool_text.push_str(&format!("  Status: {message}\n"));
                    }
                    
                    if cursor_y > 0 {
                        let _ = measure_and_render(&tool_text, &mut scratch, &mut cursor_y);
                    }
                }
            } else if let Some(live_markdown) = live_block.get_markdown_content() {
                if !live_markdown.trim().is_empty() && cursor_y > 0 {
                    let _ = measure_and_render(&live_markdown, &mut scratch, &mut cursor_y);
                }
            }
        }

        // 2) Render finalized blocks from newest to oldest above live until we filled enough
        for md_src in self.finalized_blocks.iter().rev() {
            if cursor_y == 0 {
                break;
            }
            // stop if we already filled enough (available + headroom)
            let filled = scratch_height - cursor_y;
            if filled >= available + headroom {
                break;
            }
            let _ = measure_and_render(md_src, &mut scratch, &mut cursor_y);
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

/// Check if a parameter should be rendered full-width
fn is_full_width_parameter(tool_name: &str, param_name: &str) -> bool {
    match (tool_name, param_name) {
        // Diff-style parameters
        ("replace_in_file", "diff") => true,
        ("edit", "old_text") => true,
        ("edit", "new_text") => true,
        // Content parameters
        ("write_file", "content") => true,
        // Large text parameters
        (_, "content") if param_name != "message" => true, // Exclude short message content
        (_, "output") => true,
        (_, "query") => true,
        _ => false,
    }
}

/// Check if a parameter should be hidden
fn should_hide_parameter(tool_name: &str, param_name: &str, param_value: &str) -> bool {
    match (tool_name, param_name) {
        (_, "project") => {
            // Hide project parameter if it's empty or matches common defaults
            param_value.is_empty() || param_value == "." || param_value == "unknown"
        }
        _ => false,
    }
}
