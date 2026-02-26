use anyhow::Result;
use ratatui::{
    prelude::*,
    widgets::{Paragraph, Wrap},
};
use tui_markdown as md;

use super::textarea::TextArea;

use super::composer::Composer;
use super::custom_terminal;
use super::message::{LiveMessage, MessageBlock, PlainTextBlock, ToolUseBlock};
use super::streaming::controller::{DrainedLines, StreamKind, StreamingController};
use super::transcript::TranscriptState;
use crate::types::{PlanItemStatus, PlanState};
use crate::ui::ToolStatus;
use std::time::Instant;
use tracing::{debug, info, trace, warn};

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

/// Handles the terminal display and rendering using ratatui.
/// Does NOT own a terminal — the `Tui` orchestration layer owns it.
pub struct TerminalRenderer {
    /// Transcript model with committed history and one active streaming message.
    pub transcript: TranscriptState,
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
    /// When overlay is active, history commits are deferred and flushed on close.
    overlay_active: bool,
    /// Buffered history lines emitted while overlay is active.
    deferred_history_lines: Vec<Line<'static>>,
    /// History lines ready to be inserted into terminal scrollback.
    /// Drained by the Tui orchestration layer before each draw cycle.
    pending_history_lines: Vec<Line<'static>>,

    /// Bottom composer rendering and sizing.
    composer: Composer,
    /// Queue of incoming stream deltas, drained on render commit ticks.
    streaming_controller: StreamingController,
    /// True while actively receiving stream deltas for the current assistant turn.
    streaming_open: bool,
    /// Last stream kind seen from incoming deltas (used as ordering tiebreaker).
    last_stream_kind: Option<StreamKind>,
    /// Spinner state for loading indication
    spinner_state: SpinnerState,
    /// Tracks the last block type for hidden tool paragraph breaks
    last_block_type_for_hidden_tool: Option<LastBlockType>,
    /// Flag indicating a hidden tool completed and we may need a paragraph break
    needs_paragraph_break_after_hidden_tool: bool,
    /// Last known terminal width (updated in prepare(), used for history rendering).
    last_known_width: u16,
}

/// Tracks the last block type for paragraph breaks after hidden tools
#[derive(Debug, Clone, Copy, PartialEq)]
enum LastBlockType {
    PlainText,
    Thinking,
}

/// Type alias for the production terminal renderer (no longer generic).
pub type ProductionTerminalRenderer = TerminalRenderer;

impl TerminalRenderer {
    pub fn new() -> Result<Self> {
        Ok(Self {
            transcript: TranscriptState::new(),
            pending_user_message: None,
            current_error: None,
            info_message: None,

            plan_state: None,
            plan_expanded: false,
            overlay_active: false,
            deferred_history_lines: Vec::new(),
            pending_history_lines: Vec::new(),
            composer: Composer::new(5),
            streaming_controller: StreamingController::new(),
            streaming_open: false,
            last_stream_kind: None,
            spinner_state: SpinnerState::Hidden,
            last_block_type_for_hidden_tool: None,
            needs_paragraph_break_after_hidden_tool: false,
            last_known_width: 80,
        })
    }

    /// Start a new message (called on StreamingStarted)
    pub fn start_new_message(&mut self, _request_id: u64) {
        // Flush any buffered tail chunks into the currently active message before
        // rotating the transcript lifecycle.
        let pending = self.streaming_controller.flush_pending();
        self.apply_drained_lines(pending);
        self.sync_live_stream_tails();

        // Show loading spinner
        self.spinner_state = SpinnerState::Loading {
            start_time: Instant::now(),
        };
        self.streaming_controller.clear();
        self.last_stream_kind = None;
        self.transcript.start_active_message();
        self.streaming_open = true;
    }

    /// Start a new tool use block within the current message
    pub fn start_tool_use_block(&mut self, name: String, id: String) {
        // Hide spinner when first content arrives
        self.hide_loading_spinner_if_active();

        // Flush any in-progress streaming text/thinking to scrollback so
        // the tool block in the live viewport doesn't overlap with it.
        // Also insert a blank separator so the scrollback content is visually
        // separated from the tool block that will appear in the viewport.
        if self.last_stream_kind.is_some() {
            self.flush_streaming_pending();
            self.insert_or_defer_history_lines(vec![Line::from("")]);
            if let Some(msg) = self.transcript.active_message_mut() {
                msg.streamed_to_scrollback = true;
            }
            self.last_stream_kind = None;
        }

        self.ensure_active_message();
        let Some(live_message) = self.transcript.active_message_mut() else {
            return;
        };

        live_message.add_block(MessageBlock::ToolUse(ToolUseBlock::new(name, id)));
    }

    /// Ensure the last block in the live message is of the specified type.
    #[cfg_attr(not(test), allow(dead_code))]
    /// If not, append a new block of that type
    /// Returns true if a new block was created, false if the last block was already the right type
    pub fn ensure_last_block_type(&mut self, block: MessageBlock) -> bool {
        self.ensure_active_message();

        // Determine the block type for hidden tool tracking
        let current_block_type = match &block {
            MessageBlock::PlainText(_) => Some(LastBlockType::PlainText),
            MessageBlock::Thinking(_) => Some(LastBlockType::Thinking),
            _ => None,
        };

        // Check if we need to insert a paragraph break after a hidden tool
        if self.needs_paragraph_break_after_hidden_tool {
            if let (Some(last_type), Some(current_type)) =
                (self.last_block_type_for_hidden_tool, current_block_type)
            {
                if last_type == current_type {
                    // Same type as before the hidden tool - insert paragraph break
                    if let Some(live_message) = self.transcript.active_message_mut() {
                        if let Some(last_block) = live_message.blocks.last_mut() {
                            match last_block {
                                MessageBlock::PlainText(text_block) => {
                                    text_block.content.push_str("\n\n");
                                }
                                MessageBlock::Thinking(thinking_block) => {
                                    thinking_block.content.push_str("\n\n");
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            self.needs_paragraph_break_after_hidden_tool = false;
        }

        // Track the block type for future hidden tool events
        if let Some(block_type) = current_block_type {
            self.last_block_type_for_hidden_tool = Some(block_type);
        }

        let Some(live_message) = self.transcript.active_message_mut() else {
            return false;
        };

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

    /// Mark that a hidden tool completed - paragraph break may be needed before next text
    pub fn mark_hidden_tool_completed(&mut self) {
        self.needs_paragraph_break_after_hidden_tool = true;
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

    /// Toggle whether an overlay is active (drives deferred history behavior).
    pub fn set_overlay_active(&mut self, active: bool) {
        self.overlay_active = active;
    }

    /// Append text to the last block in the current message
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn append_to_live_block(&mut self, text: &str) {
        // Hide spinner when first content arrives
        self.hide_loading_spinner_if_active();
        self.ensure_active_message();
        let Some(live_message) = self.transcript.active_message_mut() else {
            return;
        };

        if let Some(last_block) = live_message.get_last_block_mut() {
            last_block.append_content(text);
        }
    }

    /// Queue a text delta for commit-tick-based streaming.
    pub fn queue_text_delta(&mut self, content: String) {
        if !self.streaming_open {
            if self.transcript.active_message().is_none() {
                warn!(
                    "Received text delta before StreamingStarted; recovering with synthetic stream start"
                );
                self.start_new_message(0);
            } else {
                tracing::warn!("Dropping text delta while no active stream is open");
                return;
            }
        }
        // When switching from thinking to text, flush the thinking stream
        // so its tail goes to scrollback immediately rather than lingering
        // in the viewport.
        if self.last_stream_kind == Some(StreamKind::Thinking) {
            let flushed_thinking = self.streaming_controller.flush_kind(StreamKind::Thinking);
            if !flushed_thinking.is_empty() {
                let lines = style_thinking_lines(flushed_thinking);
                self.insert_or_defer_history_lines(indent_lines(lines));
                // Blank line between thinking and text blocks
                self.insert_or_defer_history_lines(vec![Line::from("")]);
                if let Some(msg) = self.transcript.active_message_mut() {
                    msg.streamed_to_scrollback = true;
                }
            }
        }
        self.last_stream_kind = Some(StreamKind::Text);
        self.streaming_controller.push(StreamKind::Text, content);
    }

    /// Queue a thinking delta for commit-tick-based streaming.
    pub fn queue_thinking_delta(&mut self, content: String) {
        if !self.streaming_open {
            if self.transcript.active_message().is_none() {
                warn!(
                    "Received thinking delta before StreamingStarted; recovering with synthetic stream start"
                );
                self.start_new_message(0);
            } else {
                tracing::warn!("Dropping thinking delta while no active stream is open");
                return;
            }
        }
        // When switching from text to thinking, flush the text stream
        // so its tail goes to scrollback immediately.
        if self.last_stream_kind == Some(StreamKind::Text) {
            let flushed_text = self.streaming_controller.flush_kind(StreamKind::Text);
            if !flushed_text.is_empty() {
                self.insert_or_defer_history_lines(indent_lines(flushed_text));
                // Blank line between text and thinking blocks
                self.insert_or_defer_history_lines(vec![Line::from("")]);
                if let Some(msg) = self.transcript.active_message_mut() {
                    msg.streamed_to_scrollback = true;
                }
            }
        }
        self.last_stream_kind = Some(StreamKind::Thinking);
        self.streaming_controller
            .push(StreamKind::Thinking, content);
    }

    /// Force-flush pending stream tails and queued chunks.
    pub fn flush_streaming_pending(&mut self) {
        let flushed = self.streaming_controller.flush_pending();
        self.apply_drained_lines(flushed);
        self.sync_live_stream_tails();
        self.streaming_open = false;
    }

    /// Add or update a tool parameter in the current message
    pub fn add_or_update_tool_parameter(&mut self, tool_id: &str, name: String, value: String) {
        let Some(live_message) = self.transcript.active_message_mut() else {
            tracing::warn!("Ignoring tool parameter update without active message");
            return;
        };

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
        let Some(live_message) = self.transcript.active_message_mut() else {
            tracing::warn!("Ignoring tool status update without active message");
            return;
        };

        if let Some(tool_block) = live_message.get_tool_block_mut(tool_id) {
            tool_block.status = status;
            tool_block.status_message = message;
            tool_block.output = output;
        }
    }

    /// Append streaming output to a tool block (used by execute_command).
    pub fn append_tool_output(&mut self, tool_id: &str, chunk: &str) {
        let Some(live_message) = self.transcript.active_message_mut() else {
            tracing::warn!("Ignoring tool output append without active message");
            return;
        };

        if let Some(tool_block) = live_message.get_tool_block_mut(tool_id) {
            match &mut tool_block.output {
                Some(existing) => existing.push_str(chunk),
                None => tool_block.output = Some(chunk.to_string()),
            }
        }
    }

    /// Add a user message as finalized message and clear any pending user message.
    /// Before adding, finalizes any active streaming message so it appears in
    /// scrollback history BEFORE this user message (correct chronological order).
    pub fn add_user_message(&mut self, content: &str) -> Result<()> {
        // Finalize any active streaming message first so it gets committed
        // to history BEFORE this user message
        self.flush_streaming_pending();
        self.transcript.finalize_active_if_content();
        // Clear stale stream state so prepare()/sync_live_stream_tails() won't
        // re-create a phantom active message from leftover tail text.
        self.streaming_controller.clear();
        self.last_stream_kind = None;
        // Flush the now-finalized agent response into scrollback
        self.flush_new_finalized_messages(self.last_known_width);

        // Create a finalized message with a UserText block for proper styling
        let mut user_message = LiveMessage::new();
        let mut text_block = PlainTextBlock::new();
        text_block.content = content.to_string();
        user_message.add_block(MessageBlock::UserText(text_block));
        user_message.finalized = true;

        self.transcript.push_committed_message(user_message);
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

        self.transcript.push_committed_message(instruction_message);
        Ok(())
    }

    /// Add pre-styled lines directly to the pending history buffer.
    /// Used for the welcome banner which needs custom styling beyond what
    /// markdown rendering provides.
    pub fn add_styled_history_lines(&mut self, lines: Vec<Line<'static>>) {
        self.pending_history_lines.extend(lines);
    }

    /// Clear all messages and reset state
    pub fn clear_all_messages(&mut self) {
        self.transcript.clear();
        self.streaming_controller.clear();
        self.streaming_open = false;
        self.last_stream_kind = None;
        self.deferred_history_lines.clear();
        self.pending_history_lines.clear();
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

    fn flush_deferred_history_lines(&mut self) {
        if self.deferred_history_lines.is_empty() {
            return;
        }

        let lines = std::mem::take(&mut self.deferred_history_lines);
        self.insert_or_defer_history_lines(lines);
    }

    fn flush_new_finalized_messages(&mut self, width: u16) {
        let unrendered = self.transcript.unrendered_committed_messages();
        if unrendered.is_empty() {
            return;
        }

        let mut lines = Vec::new();
        for message in unrendered {
            if message.streamed_to_scrollback {
                // PlainText and Thinking blocks were already progressively sent
                // to scrollback during streaming. Only send non-streamed blocks
                // (ToolUse, UserText) that were added directly to the message.
                let tool_lines =
                    TranscriptState::as_history_lines_non_streamed_only(message, width);
                if !tool_lines.is_empty() {
                    // The blank separator before these tool blocks was already
                    // inserted by start_tool_use_block when it flushed the
                    // preceding streamed content.
                    lines.extend(tool_lines);
                    // Trailing blank so the next streamed content doesn't
                    // visually merge with the tool block.
                    lines.push(Line::from(""));
                }
                continue;
            }
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }
            lines.extend(TranscriptState::as_history_lines(message, width));
        }

        self.insert_or_defer_history_lines(lines);
        self.transcript.mark_committed_as_rendered();
    }

    fn apply_streaming_commit_tick(&mut self) {
        let drained = self.streaming_controller.drain_commit_tick();
        self.apply_drained_lines(drained);
        self.sync_live_stream_tails();
    }

    fn apply_drained_lines(&mut self, drained: DrainedLines) {
        if !drained.text.is_empty() || !drained.thinking.is_empty() {
            info!(
                target: "tui_scrollback",
                text_lines = drained.text.len(),
                thinking_lines = drained.thinking.len(),
                overlay_active = self.overlay_active,
                streaming_open = self.streaming_open,
                "apply drained lines"
            );
        }

        // Drained lines always go to scrollback — both during streaming and after.
        // During streaming, the viewport shows ONLY the undrained tail (via
        // sync_live_stream_tails), so there is no duplication.
        let has_lines = !drained.text.is_empty() || !drained.thinking.is_empty();

        if !drained.text.is_empty() {
            self.insert_or_defer_history_lines(indent_lines(drained.text));
        }

        if !drained.thinking.is_empty() {
            let lines = style_thinking_lines(drained.thinking);
            self.insert_or_defer_history_lines(indent_lines(lines));
        }

        // Mark the active message so flush_new_finalized_messages() won't
        // re-send text/thinking content that was already sent to scrollback.
        if self.streaming_open && has_lines {
            if let Some(msg) = self.transcript.active_message_mut() {
                msg.streamed_to_scrollback = true;
            }
        }
    }

    fn sync_live_stream_tails(&mut self) {
        // The viewport shows ONLY the undrained tail — all committed (drained)
        // content has already been sent to scrollback.
        let text_tail = self.streaming_controller.tail_text(StreamKind::Text);
        let thinking_tail = self.streaming_controller.tail_text(StreamKind::Thinking);
        trace!(
            target: "tui_scrollback",
            text_tail_len = text_tail.len(),
            thinking_tail_len = thinking_tail.len(),
            "sync live tails (tail-only)"
        );

        if text_tail.is_empty() && thinking_tail.is_empty() {
            // All content has been drained to scrollback. Only remove stream
            // blocks when the streaming controller actually managed the content
            // (has_seen_any_delta). If blocks were added directly via
            // append_to_live_block, they should not be stripped.
            if self.streaming_controller.has_seen_any_delta() {
                if let Some(live_message) = self.transcript.active_message_mut() {
                    live_message
                        .blocks
                        .retain(|block| stream_kind_for_block(block).is_none());
                }
            }
            return;
        }

        self.ensure_active_message();
        let Some(live_message) = self.transcript.active_message_mut() else {
            return;
        };

        let text_content = text_tail.to_string();
        let thinking_content = thinking_tail.to_string();
        let stream_blocks = build_stream_blocks_for_live_message(
            &live_message.blocks,
            text_content,
            thinking_content,
            self.last_stream_kind,
        );
        if stream_blocks.is_empty() {
            return;
        }

        live_message.blocks =
            merge_blocks_preserving_stream_slots(&live_message.blocks, stream_blocks);
    }

    fn insert_or_defer_history_lines(&mut self, lines: Vec<Line<'static>>) {
        if lines.is_empty() {
            return;
        }

        if self.overlay_active {
            self.deferred_history_lines.extend(lines);
            return;
        }

        self.pending_history_lines.extend(lines);
    }

    /// Drain pending history lines for the Tui layer to insert into scrollback.
    pub fn drain_pending_history_lines(&mut self) -> Vec<Line<'static>> {
        std::mem::take(&mut self.pending_history_lines)
    }

    /// Prepare for the next frame: flush streaming data, commit finalized messages.
    /// Must be called before `paint()` each frame.
    pub fn prepare(&mut self, width: u16, screen_height: u16) {
        let _ = screen_height; // Reserved for future partial-scrollback support
        self.last_known_width = width;
        // Account for 2-char indent when computing streaming wrap width
        let stream_width = width.saturating_sub(2).max(1) as usize;
        self.streaming_controller.set_width(Some(stream_width));
        self.apply_streaming_commit_tick();
        if !self.overlay_active {
            self.flush_deferred_history_lines();
        }
        self.flush_new_finalized_messages(width);
    }

    /// Compute the desired viewport height for the current content.
    pub fn desired_viewport_height(&self, textarea: &TextArea, screen_width: u16) -> u16 {
        let input_height = self.composer.calculate_input_height(textarea, screen_width);
        let mut content_height: u16 = 0;

        // Live message height
        if let Some(live_message) = self.transcript.active_message() {
            if live_message.has_content() {
                for block in &live_message.blocks {
                    content_height = content_height
                        .saturating_add(block.calculate_height(screen_width))
                        .saturating_add(1); // gap between blocks
                }
            }
        }

        // Spinner height
        if self.spinner_state.get_spinner_char().is_some() {
            content_height = content_height.saturating_add(2); // spinner + gap
        }

        // Status/error height
        content_height = content_height.saturating_add(self.measure_status_height(screen_width));

        // Always reserve at least 1 row so there's a visible gap between
        // scrollback and the composer when no live content is displayed.
        content_height = content_height.max(1);

        content_height.saturating_add(input_height)
    }

    fn measure_status_height(&self, width: u16) -> u16 {
        let mut height: u16 = 0;
        if self.current_error.is_some() {
            let formatted = Self::format_error_message(self.current_error.as_deref().unwrap());
            height = Self::measure_markdown_height(&formatted, width, 20);
            if height > 0 {
                height = height.saturating_add(1); // gap
            }
        } else {
            let mut has_any = false;
            if let Some(plan_text) = self.build_plan_text() {
                let h = Self::measure_markdown_height(&plan_text, width, 20);
                height = height.saturating_add(h);
                has_any = true;
            }
            if let Some(ref info_msg) = self.info_message {
                if has_any {
                    height = height.saturating_add(1);
                }
                let h = Self::measure_markdown_height(info_msg, width, 20);
                height = height.saturating_add(h);
                has_any = true;
            } else if let Some(ref pending_msg) = self.pending_user_message {
                if has_any {
                    height = height.saturating_add(1);
                }
                let h = Self::measure_markdown_height(pending_msg, width, 20);
                height = height.saturating_add(h);
                has_any = true;
            }
            if has_any {
                height = height.saturating_add(1); // gap above status
            }
        }
        height
    }

    /// Paint the current state into the provided frame.
    /// The frame area is the viewport area provided by Tui.
    pub fn paint(&mut self, f: &mut custom_terminal::Frame, textarea: &TextArea) {
        let full = f.area();
        let width = full.width;
        let input_height = self.composer.calculate_input_height(textarea, width);
        let available = full.height.saturating_sub(input_height);

        let headroom: u16 = 200;
        let scratch_height = available.saturating_add(headroom).max(available);
        let mut scratch = Buffer::empty(Rect::new(0, 0, width, scratch_height));

        let mut cursor_y = scratch_height;

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

                scratch.set_string(
                    2,
                    cursor_y,
                    spinner_char.to_string(),
                    Style::default().fg(spinner_color),
                );

                if let Some(status_text) = self.spinner_state.get_status_text() {
                    scratch.set_string(
                        4,
                        cursor_y,
                        &status_text,
                        Style::default().fg(Color::LightRed),
                    );
                }

                cursor_y = cursor_y.saturating_sub(1);
            }
        }

        // 2) Render current live message (so it is closest to the input)
        if let Some(live_message) = self.transcript.active_message() {
            if live_message.has_content() && cursor_y > 0 {
                self.render_message_to_buffer(live_message, &mut scratch, &mut cursor_y, width);
                cursor_y = cursor_y.saturating_sub(1);
            }
        }

        // Composed content occupies rows [cursor_y .. scratch_height)
        let total_height = scratch_height.saturating_sub(cursor_y);

        let [content_area, status_area, input_area] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(status_height),
            Constraint::Length(input_height),
        ])
        .areas(full);

        let visible_total = total_height.min(content_area.height);
        let top_blank = content_area.height - visible_total;
        let visible_start = scratch_height.saturating_sub(visible_total);
        let dst = f.buffer_mut();

        // Top blank area (if any)
        for y in 0..top_blank {
            for x in 0..content_area.width {
                if let Some(cell) = dst.cell_mut((content_area.x + x, content_area.y + y)) {
                    cell.set_style(Style::default());
                    cell.set_char(' ');
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
                    if src.symbol().is_empty() {
                        dst_cell.set_style(Style::default());
                        dst_cell.set_char(' ');
                    } else {
                        *dst_cell = src;
                    }
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
        self.composer.render(f, input_area, textarea);
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

    fn render_status_entries(f: &mut custom_terminal::Frame, area: Rect, entries: &[StatusEntry]) {
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

    fn render_info_message(f: &mut custom_terminal::Frame, area: Rect, message: &str) {
        if area.height == 0 {
            return;
        }

        let text = md::from_str(message);
        let paragraph = Paragraph::new(text)
            .style(Style::default().fg(Color::Gray))
            .wrap(Wrap { trim: false });

        f.render_widget(paragraph, area);
    }

    fn render_plan_message(f: &mut custom_terminal::Frame, area: Rect, plan_text: &str) {
        if area.height == 0 {
            return;
        }

        let text = md::from_str(plan_text);
        let paragraph = Paragraph::new(text)
            .style(Style::default().fg(Color::Gray).add_modifier(Modifier::DIM))
            .wrap(Wrap { trim: false });

        f.render_widget(paragraph, area);
    }

    fn clear_status_gap(f: &mut custom_terminal::Frame, area: Rect) {
        if area.height == 0 {
            return;
        }

        let buffer = f.buffer_mut();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if let Some(cell) = buffer.cell_mut((x, y)) {
                    cell.set_style(Style::default());
                    cell.set_char(' ');
                }
            }
        }
    }

    /// Calculate the height needed for the input area based on textarea content
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn calculate_input_height(&self, textarea: &TextArea, width: u16) -> u16 {
        self.composer.calculate_input_height(textarea, width)
    }

    #[cfg(test)]
    fn max_input_rows(&self) -> u16 {
        self.composer.max_input_rows()
    }

    /// Render pending user message with dimmed and italic styling
    fn render_pending_message(f: &mut custom_terminal::Frame, area: Rect, message: &str) {
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
    fn render_error_message(f: &mut custom_terminal::Frame, area: Rect, message: &str) {
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

    /// Returns true when the UI has time-varying content that requires
    /// periodic redraws even without external events (spinner animation,
    /// streaming commit ticks).
    pub fn needs_animation_timer(&self) -> bool {
        !matches!(self.spinner_state, SpinnerState::Hidden) || self.streaming_open
    }

    /// Set an info message to display
    pub fn set_info(&mut self, info_message: String) {
        self.info_message = Some(info_message);
    }

    /// Clear the current info message
    pub fn clear_info(&mut self) {
        self.info_message = None;
    }

    fn ensure_active_message(&mut self) {
        if self.transcript.active_message().is_none() {
            tracing::warn!("Recovering missing active message in renderer");
            self.transcript.start_active_message();
        }
    }

    #[cfg(test)]
    fn deferred_history_line_count(&self) -> usize {
        self.deferred_history_lines.len()
    }
}

/// Apply Yellow+Italic style to thinking lines while preserving per-span markdown styling.
fn style_thinking_lines(thinking: Vec<Line<'static>>) -> Vec<Line<'static>> {
    thinking
        .into_iter()
        .map(|line| {
            let styled_spans: Vec<Span<'static>> = line
                .spans
                .into_iter()
                .map(|span| {
                    let style = span
                        .style
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM)
                        .add_modifier(Modifier::ITALIC);
                    Span::styled(span.content.to_string(), style)
                })
                .collect();
            Line::from(styled_spans)
        })
        .collect()
}

/// Prepend a 2-space indent to each line so scrollback content aligns with
/// the user's "› " prefix.
fn indent_lines(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|mut line| {
            line.spans.insert(0, Span::raw("  ".to_string()));
            line
        })
        .collect()
}

fn stream_kind_for_block(block: &MessageBlock) -> Option<StreamKind> {
    match block {
        MessageBlock::PlainText(_) => Some(StreamKind::Text),
        MessageBlock::Thinking(_) => Some(StreamKind::Thinking),
        MessageBlock::ToolUse(_) | MessageBlock::UserText(_) => None,
    }
}

fn block_for_stream_kind(kind: StreamKind, content: String) -> Option<MessageBlock> {
    match kind {
        StreamKind::Text => {
            if content.is_empty() {
                return None;
            }
            let mut block = PlainTextBlock::new();
            block.content = content;
            Some(MessageBlock::PlainText(block))
        }
        StreamKind::Thinking => {
            if content.trim().is_empty() {
                return None;
            }
            let mut block = super::message::ThinkingBlock::new();
            block.content = content;
            Some(MessageBlock::Thinking(block))
        }
    }
}

fn build_stream_blocks_for_live_message(
    existing_blocks: &[MessageBlock],
    text_content: String,
    thinking_content: String,
    last_stream_kind: Option<StreamKind>,
) -> Vec<MessageBlock> {
    let mut order = existing_blocks
        .iter()
        .filter_map(stream_kind_for_block)
        .collect::<Vec<_>>();

    if order.is_empty() {
        match last_stream_kind {
            Some(StreamKind::Text) => {
                if !thinking_content.trim().is_empty() {
                    order.push(StreamKind::Thinking);
                }
                if !text_content.is_empty() {
                    order.push(StreamKind::Text);
                }
            }
            Some(StreamKind::Thinking) => {
                if !text_content.is_empty() {
                    order.push(StreamKind::Text);
                }
                if !thinking_content.trim().is_empty() {
                    order.push(StreamKind::Thinking);
                }
            }
            None => {
                if !text_content.is_empty() {
                    order.push(StreamKind::Text);
                }
                if !thinking_content.trim().is_empty() {
                    order.push(StreamKind::Thinking);
                }
            }
        }
    } else {
        if !text_content.is_empty() && !order.contains(&StreamKind::Text) {
            order.push(StreamKind::Text);
        }
        if !thinking_content.trim().is_empty() && !order.contains(&StreamKind::Thinking) {
            order.push(StreamKind::Thinking);
        }
    }

    let mut out = Vec::new();
    for kind in order {
        let content = match kind {
            StreamKind::Text => text_content.clone(),
            StreamKind::Thinking => thinking_content.clone(),
        };
        if let Some(block) = block_for_stream_kind(kind, content) {
            out.push(block);
        }
    }
    out
}

fn merge_blocks_preserving_stream_slots(
    existing_blocks: &[MessageBlock],
    stream_blocks: Vec<MessageBlock>,
) -> Vec<MessageBlock> {
    let mut rebuilt = Vec::with_capacity(existing_blocks.len().max(stream_blocks.len()));
    let mut stream_iter = stream_blocks.into_iter();

    for block in existing_blocks {
        if stream_kind_for_block(block).is_some() {
            if let Some(next_stream_block) = stream_iter.next() {
                rebuilt.push(next_stream_block);
            }
            continue;
        }
        rebuilt.push(block.clone());
    }

    // Append any remaining stream blocks (e.g. new stream types that didn't
    // exist in the previous set of blocks).
    rebuilt.extend(stream_iter);

    rebuilt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PlanItem, PlanItemStatus, PlanState};
    use crate::ui::terminal::message::{LiveMessage, MessageBlock, PlainTextBlock};

    /// Test harness that provides a TerminalRenderer and a buffer to render into.
    /// This replaces the old approach where TerminalRenderer owned a Terminal<TestBackend>.
    struct TestHarness {
        renderer: TerminalRenderer,
        width: u16,
        height: u16,
        /// The last rendered buffer (filled by `render()`).
        buffer: Buffer,
    }

    impl TestHarness {
        fn new(width: u16, height: u16) -> Self {
            Self {
                renderer: TerminalRenderer::new().unwrap(),
                width,
                height,
                buffer: Buffer::empty(Rect::new(0, 0, width, height)),
            }
        }

        /// Render the UI into the internal buffer. Returns a reference to the buffer.
        /// Note: does NOT drain pending history lines — call `drain_pending_history_lines()`
        /// separately if you want to inspect them.
        fn render(&mut self, textarea: &TextArea) -> &Buffer {
            let area = Rect::new(0, 0, self.width, self.height);
            self.buffer = Buffer::empty(area);
            self.renderer.prepare(self.width, self.height);
            let mut frame = custom_terminal::Frame {
                cursor_position: None,
                viewport_area: area,
                buffer: &mut self.buffer,
            };
            self.renderer.paint(&mut frame, textarea);
            &self.buffer
        }

        /// Access the buffer after rendering
        fn buffer(&self) -> &Buffer {
            &self.buffer
        }
    }

    impl std::ops::Deref for TestHarness {
        type Target = TerminalRenderer;
        fn deref(&self) -> &Self::Target {
            &self.renderer
        }
    }

    impl std::ops::DerefMut for TestHarness {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.renderer
        }
    }

    fn create_test_harness(width: u16, height: u16) -> TestHarness {
        TestHarness::new(width, height)
    }

    fn create_default_test_harness() -> TestHarness {
        create_test_harness(80, 20)
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
            let renderer = create_default_test_harness();
            assert_eq!(renderer.transcript.committed_messages().len(), 0);
            assert!(renderer.transcript.active_message().is_none());
            assert!(!renderer.has_error());
        }

        #[test]
        fn test_message_finalization_workflow() {
            let mut renderer = create_default_test_harness();

            // Start a new message
            renderer.start_new_message(1);
            assert!(renderer.transcript.active_message().is_some());
            assert_eq!(renderer.transcript.committed_messages().len(), 0);

            // Add content to live message
            renderer.ensure_last_block_type(MessageBlock::PlainText(PlainTextBlock::new()));
            renderer.append_to_live_block("Test content");

            // Verify live message has content
            let live_message = renderer.transcript.active_message().unwrap();
            assert!(live_message.has_content());
            assert!(!live_message.finalized);

            // Start another message - should finalize the previous one
            renderer.start_new_message(2);

            // Previous message should be finalized
            assert_eq!(renderer.transcript.committed_messages().len(), 1);
            assert!(renderer.transcript.committed_messages()[0].finalized);
            assert!(renderer.transcript.committed_messages()[0].has_content());

            // New live message should be empty
            let new_live = renderer.transcript.active_message().unwrap();
            assert!(!new_live.has_content());
            assert!(!new_live.finalized);
        }

        #[test]
        fn test_ensure_last_block_type_behavior() {
            let mut renderer = create_default_test_harness();

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
            let live_message = renderer.transcript.active_message().unwrap();
            assert_eq!(live_message.blocks.len(), 2);
        }

        #[test]
        fn test_content_appending_to_blocks() {
            let mut renderer = create_default_test_harness();

            // Start a message
            renderer.start_new_message(1);

            // Add a text block and append content
            renderer.ensure_last_block_type(MessageBlock::PlainText(PlainTextBlock::new()));
            renderer.append_to_live_block("Hello ");
            renderer.append_to_live_block("world!");

            // Verify content was appended
            let live_message = renderer.transcript.active_message().unwrap();
            assert_eq!(live_message.blocks.len(), 1);

            if let MessageBlock::PlainText(text_block) = &live_message.blocks[0] {
                assert_eq!(text_block.content, "Hello world!");
            } else {
                panic!("Expected PlainText block");
            }
        }

        #[test]
        fn test_pending_message_rendering() {
            let mut renderer = create_default_test_harness();
            let textarea = TextArea::new();

            // Initially no pending message - should render only input area
            renderer.render(&textarea);
            let buffer = renderer.buffer();

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
            renderer.render(&textarea);
            let buffer = renderer.buffer();

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
            renderer.render(&textarea);
            let buffer = renderer.buffer();

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
            let mut renderer = create_default_test_harness();
            let textarea = TextArea::new();

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

            renderer.render(&textarea);
            let buffer = renderer.buffer();

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
            let mut renderer = create_default_test_harness();
            renderer.set_plan_expanded(true);
            let textarea = TextArea::new();

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

            renderer.render(&textarea);
            let buffer = renderer.buffer();

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
            let mut renderer = create_default_test_harness();
            let textarea = TextArea::new();

            // Initially no error - should render cleanly
            renderer.render(&textarea);
            let buffer = renderer.buffer();

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
            renderer.render(&textarea);
            let buffer = renderer.buffer();

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
            renderer.render(&textarea);
            let buffer = renderer.buffer();

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
        fn test_overlay_defers_and_flushes_committed_history_lines() {
            let mut renderer = create_default_test_harness();
            let textarea = TextArea::new();

            renderer.set_plan_expanded(true);
            renderer.set_overlay_active(true);
            renderer.start_new_message(1);
            renderer.queue_text_delta("deferred line\n".to_string());
            renderer.render(&textarea);

            renderer.start_new_message(2);
            renderer.render(&textarea);

            assert!(
                renderer.deferred_history_line_count() > 0,
                "History commits should be buffered while overlay is active"
            );
            renderer.set_plan_expanded(false);
            renderer.set_overlay_active(false);
            renderer.render(&textarea);

            assert_eq!(renderer.deferred_history_line_count(), 0);
        }

        #[test]
        fn test_overlay_deferral_survives_resize_until_close() {
            let mut renderer = create_default_test_harness();
            let textarea = TextArea::new();

            renderer.set_overlay_active(true);
            renderer.start_new_message(1);
            renderer.queue_text_delta("resize defer\n".to_string());
            renderer.render(&textarea);

            renderer.start_new_message(2);
            renderer.render(&textarea);

            assert!(renderer.deferred_history_line_count() > 0);

            // Simulate resize by just re-rendering (Tui handles resize now)
            renderer.render(&textarea);

            assert!(
                renderer.deferred_history_line_count() > 0,
                "Resize must not flush deferred history while overlay is active"
            );

            renderer.set_overlay_active(false);
            renderer.render(&textarea);
            assert_eq!(renderer.deferred_history_line_count(), 0);
        }

        #[test]
        fn test_overlay_defers_tool_event_history_and_flushes_on_close() {
            let mut renderer = create_default_test_harness();
            let textarea = TextArea::new();

            renderer.start_new_message(1);
            renderer.start_tool_use_block("shell".to_string(), "tool-1".to_string());
            renderer.add_or_update_tool_parameter(
                "tool-1",
                "command".to_string(),
                "echo hi".to_string(),
            );
            renderer.update_tool_status(
                "tool-1",
                ToolStatus::Success,
                Some("done".to_string()),
                Some("hi".to_string()),
            );

            renderer.set_overlay_active(true);
            renderer.start_new_message(2);
            renderer.render(&textarea);

            assert_eq!(renderer.transcript.committed_messages().len(), 1);
            assert!(
                renderer.deferred_history_line_count() > 0,
                "Tool history should be deferred while overlay is active"
            );

            renderer.set_overlay_active(false);
            renderer.render(&textarea);
            assert_eq!(renderer.deferred_history_line_count(), 0);
        }

        #[test]
        fn test_late_stream_delta_after_stop_is_ignored() {
            let mut renderer = create_default_test_harness();
            let textarea = TextArea::new();

            renderer.start_new_message(1);
            renderer.flush_streaming_pending();
            renderer.queue_text_delta("late chunk".to_string());
            renderer.render(&textarea);

            let live_message = renderer.transcript.active_message().unwrap();
            assert!(
                !live_message.has_content(),
                "Late deltas after stop should not mutate the live message"
            );
        }

        #[test]
        fn test_pre_start_delta_recovers_streaming_state() {
            let mut renderer = create_default_test_harness();
            let textarea = TextArea::new();

            renderer.queue_text_delta("hello before start".to_string());
            renderer.render(&textarea);

            let live_message = renderer.transcript.active_message().unwrap();
            assert!(
                live_message.has_content(),
                "Pre-start deltas should recover by starting a synthetic stream"
            );
        }

        #[test]
        fn test_spinner_state_management() {
            let mut renderer = create_default_test_harness();

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
            let mut renderer = create_default_test_harness();

            // Add some finalized messages
            for i in 0..3 {
                let message = create_text_message(&format!("Message {i}"));
                renderer.transcript.committed_messages_mut().push(message);
            }

            // Add live message
            renderer.start_new_message(1);
            renderer.ensure_last_block_type(MessageBlock::PlainText(PlainTextBlock::new()));
            renderer.append_to_live_block("Live content");

            // Set some state
            renderer.show_rate_limit_spinner(30);

            // Clear all messages
            renderer.clear_all_messages();

            // Everything should be reset
            assert!(renderer.transcript.committed_messages().is_empty());
            assert!(renderer.transcript.active_message().is_none());
            assert!(matches!(renderer.spinner_state, SpinnerState::Hidden));
        }

        #[test]
        fn test_tool_status_updates() {
            let mut renderer = create_default_test_harness();

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
            let live_message = renderer.transcript.active_message().unwrap();
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

            // Test line wrapping (effective width is 78 due to 2-char indent)
            let long_line = "a".repeat(160); // Should wrap to 3 lines with inner width 78
            let mut wrap_block = PlainTextBlock::new();
            wrap_block.content = long_line;
            let message_block = MessageBlock::PlainText(wrap_block);
            assert_eq!(
                message_block.calculate_height(width),
                3,
                "Long line should wrap to 3 lines at inner width 78"
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

            // Test with zero width (edge case) — zero width means nothing can render
            let mut text_block = PlainTextBlock::new();
            text_block.content = "Hello".to_string();
            let message_block = MessageBlock::PlainText(text_block);
            let height = message_block.calculate_height(0);
            assert_eq!(height, 0, "Zero width should produce zero height");
        }
    }

    mod input_height_tests {
        use super::*;

        #[test]
        fn test_input_height_calculation() {
            let renderer = create_default_test_harness();
            let width = 80;

            // Test empty textarea
            // Layout: 1 top + 1 textarea + 1 bottom padding + 1 footer = 4
            let textarea = TextArea::new();
            let height = renderer.calculate_input_height(&textarea, width);
            assert_eq!(
                height, 4,
                "Empty textarea should have minimum height (1 top + 1 content + 1 bottom + 1 footer)"
            );

            // Test single line content
            let mut textarea = TextArea::new();
            textarea.insert_str("Hello");
            let height = renderer.calculate_input_height(&textarea, width);
            assert_eq!(height, 4, "Single line should still be minimum height");

            // Test multiple lines
            // Layout: 1 top + 3 textarea + 1 bottom padding + 1 footer = 6
            let mut textarea = TextArea::new();
            textarea.insert_str("Line 1\nLine 2\nLine 3");
            let height = renderer.calculate_input_height(&textarea, width);
            assert_eq!(
                height, 6,
                "Three lines should give height 6 (1 top + 3 content + 1 bottom + 1 footer)"
            );

            // Test max height constraint
            let mut textarea = TextArea::new();
            let many_lines = (0..10)
                .map(|i| format!("Line {i}"))
                .collect::<Vec<_>>()
                .join("\n");
            textarea.insert_str(&many_lines);
            let height = renderer.calculate_input_height(&textarea, width);
            assert_eq!(
                height,
                renderer.max_input_rows() + 3,
                "Should be capped at max_input_rows + top + bottom + footer"
            );
        }

        #[test]
        fn test_input_height_constraints() {
            let renderer = create_default_test_harness();
            let width = 80;

            // Test that height is always at least 4 (top + content + bottom + footer)
            let textarea = TextArea::new();
            let height = renderer.calculate_input_height(&textarea, width);
            assert!(height >= 4, "Height should always be at least 4");

            // Test that height never exceeds max_input_rows + 3
            let mut textarea = TextArea::new();
            let excessive_lines = (0..100)
                .map(|i| format!("Line {i}"))
                .collect::<Vec<_>>()
                .join("\n");
            textarea.insert_str(&excessive_lines);
            let height = renderer.calculate_input_height(&textarea, width);
            assert!(
                height <= renderer.max_input_rows() + 3,
                "Height should never exceed max_input_rows + 3"
            );
        }
    }

    mod integration_tests {
        use super::*;

        #[test]
        fn test_complete_message_workflow_rendering() {
            let mut renderer = create_default_test_harness();
            let textarea = TextArea::new();

            // 1. Start streaming - should show spinner
            renderer.start_new_message(1);
            renderer.render(&textarea);
            let buffer = renderer.buffer();

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
            renderer.render(&textarea);
            let buffer = renderer.buffer();

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
            renderer.render(&textarea);
            let buffer = renderer.buffer();

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
            renderer.render(&textarea);
            let buffer = renderer.buffer();

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
                    // Check if the status symbol (at col 0, no indent) is green (success)
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
            assert_eq!(renderer.transcript.committed_messages().len(), 1);
            assert!(renderer.transcript.committed_messages()[0].finalized);
        }

        #[test]
        fn test_finalized_messages_produce_pending_history_lines() {
            let mut renderer = create_test_harness(80, 10);
            let textarea = TextArea::new();

            // Add finalized messages
            for i in 0..3 {
                let message = create_text_message(&format!("Message {i}"));
                renderer.transcript.committed_messages_mut().push(message);
            }

            // Render to flush finalized messages
            renderer.render(&textarea);

            // Drain pending history lines — they should contain our messages
            let lines = renderer.drain_pending_history_lines();
            let combined: String = lines
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");

            assert!(
                combined.contains("Message 0"),
                "Pending history should contain Message 0"
            );
            assert!(
                combined.contains("Message 2"),
                "Pending history should contain Message 2"
            );

            // Second drain should be empty
            let lines2 = renderer.drain_pending_history_lines();
            assert!(
                lines2.is_empty(),
                "Pending history should be empty after drain"
            );
        }

        #[test]
        fn test_live_message_not_in_pending_history() {
            let mut renderer = create_test_harness(80, 10);
            let textarea = TextArea::new();

            // Start live message
            renderer.start_new_message(1);
            renderer.ensure_last_block_type(MessageBlock::PlainText(PlainTextBlock::new()));
            renderer.append_to_live_block("Live content should not be in history");

            renderer.render(&textarea);

            // Live message should NOT appear in pending history
            let lines = renderer.drain_pending_history_lines();
            let combined: String = lines
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");

            assert!(
                !combined.contains("Live content"),
                "Live message should not be in pending history lines"
            );
        }

        #[test]
        fn test_live_message_rendering_priority() {
            let mut renderer = create_default_test_harness();
            let textarea = TextArea::new();

            // Add some finalized messages
            for i in 0..2 {
                let message = create_text_message(&format!("Finalized message {i}"));
                renderer.transcript.committed_messages_mut().push(message);
            }

            // Start a live message
            renderer.start_new_message(1);
            renderer.ensure_last_block_type(MessageBlock::PlainText(PlainTextBlock::new()));
            renderer.append_to_live_block("This is live content being streamed");

            renderer.render(&textarea);
            let buffer = renderer.buffer();

            // Live content should appear in the viewport
            let mut found_live_content = false;
            for y in 0..18 {
                let mut line_text = String::new();
                for x in 0..80 {
                    let cell = buffer.cell((x, y)).unwrap();
                    line_text.push_str(cell.symbol());
                }

                if line_text.contains("live content being streamed") {
                    found_live_content = true;
                }
            }
            assert!(found_live_content, "Should render live content in viewport");

            // Finalized messages should be in pending history lines (scrollback),
            // NOT in the viewport buffer
            let pending = renderer.drain_pending_history_lines();
            let combined: String = pending
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                combined.contains("Finalized message"),
                "Finalized content should be in pending history lines"
            );
        }

        #[test]
        fn test_spinner_rendering_states() {
            let mut renderer = create_default_test_harness();
            let textarea = TextArea::new();

            // Test loading spinner
            renderer.start_new_message(1);
            renderer.render(&textarea);
            let buffer = renderer.buffer();

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
            renderer.render(&textarea);
            let buffer = renderer.buffer();

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
            renderer.render(&textarea);
            let buffer = renderer.buffer();

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
            let mut renderer = create_default_test_harness();
            let textarea = TextArea::new();

            // Set both pending message and error
            renderer.set_pending_user_message(Some("User is typing...".to_string()));
            renderer.set_error("Critical error occurred".to_string());

            // Render and verify error takes priority over pending message
            renderer.render(&textarea);
            let buffer = renderer.buffer();

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
            renderer.render(&textarea);
            let buffer = renderer.buffer();

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
            renderer.render(&textarea);
            let buffer = renderer.buffer();

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

        #[test]
        fn test_streamed_thinking_text_then_tool_has_single_blank_before_tool() {
            let mut renderer = create_test_harness(80, 20);
            let textarea = TextArea::new();

            // Start a message (like StreamingStarted)
            renderer.start_new_message(1);

            // Stream some thinking content
            renderer.queue_thinking_delta("Let me think about this.\n".to_string());
            // Drain via render to simulate commit tick
            renderer.render(&textarea);
            let _ = renderer.drain_pending_history_lines();

            // Switch to text — this should flush thinking + insert blank separator
            renderer.queue_text_delta("Here is my answer.\n".to_string());
            renderer.render(&textarea);
            let _ = renderer.drain_pending_history_lines();

            // Start a tool block (like UiEvent::StartTool)
            renderer.start_tool_use_block("write_file".to_string(), "tool_1".to_string());
            renderer.add_or_update_tool_parameter(
                "tool_1",
                "path".to_string(),
                "/tmp/test.txt".to_string(),
            );
            renderer.update_tool_status("tool_1", ToolStatus::Success, None, None);

            // Finalize the message (like what add_user_message does)
            renderer.flush_streaming_pending();
            renderer.transcript.finalize_active_if_content();
            renderer.render(&textarea);

            // Drain all pending history lines — these represent what goes to scrollback
            let lines = renderer.drain_pending_history_lines();

            // Debug: print all lines
            let line_strs: Vec<String> = lines
                .iter()
                .map(|l| {
                    if l.spans.is_empty() {
                        "<<blank>>".to_string()
                    } else {
                        l.spans
                            .iter()
                            .map(|s| s.content.as_ref())
                            .collect::<String>()
                    }
                })
                .collect();

            // Find the tool line (starts with "● ")
            let tool_line_idx = line_strs
                .iter()
                .position(|s| s.contains("●"))
                .expect("Should have a tool line");

            // Count consecutive blank lines immediately before the tool
            let mut blank_count = 0;
            let mut idx = tool_line_idx;
            while idx > 0 {
                idx -= 1;
                if line_strs[idx] == "<<blank>>" {
                    blank_count += 1;
                } else {
                    break;
                }
            }

            assert_eq!(
                blank_count,
                1,
                "Expected exactly 1 blank line before tool block, got {}.\nAll lines:\n{}",
                blank_count,
                line_strs
                    .iter()
                    .enumerate()
                    .map(|(i, s)| format!("  [{i:2}] {s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }

        /// Test that simulates a full flow WITHOUT draining intermediate pending
        /// history lines — this is closer to what happens when the user scrolls
        /// back and sees accumulated scrollback content.
        #[test]
        fn test_streamed_flow_accumulated_has_single_blank_before_tool() {
            let mut renderer = create_test_harness(80, 20);
            let textarea = TextArea::new();

            // Start a message (like StreamingStarted)
            renderer.start_new_message(1);

            // Stream some thinking content
            renderer.queue_thinking_delta("Let me think about this.\n".to_string());
            renderer.render(&textarea);
            // Do NOT drain — accumulate all lines

            // Switch to text
            renderer.queue_text_delta("Here is my answer.\n".to_string());
            renderer.render(&textarea);

            // Start a tool block
            renderer.start_tool_use_block("write_file".to_string(), "tool_1".to_string());
            renderer.add_or_update_tool_parameter(
                "tool_1",
                "path".to_string(),
                "/tmp/test.txt".to_string(),
            );
            renderer.update_tool_status("tool_1", ToolStatus::Success, None, None);

            // Finalize
            renderer.flush_streaming_pending();
            renderer.transcript.finalize_active_if_content();
            renderer.render(&textarea);

            // Now drain ALL accumulated history lines at once
            let lines = renderer.drain_pending_history_lines();

            let line_strs: Vec<String> = lines
                .iter()
                .map(|l| {
                    if l.spans.is_empty() {
                        "<<blank>>".to_string()
                    } else {
                        l.spans
                            .iter()
                            .map(|s| s.content.as_ref())
                            .collect::<String>()
                    }
                })
                .collect();

            // Find the tool line
            let tool_line_idx = line_strs
                .iter()
                .position(|s| s.contains("●"))
                .expect("Should have a tool line");

            // Count consecutive blank lines immediately before the tool
            let mut blank_count = 0;
            let mut idx = tool_line_idx;
            while idx > 0 {
                idx -= 1;
                if line_strs[idx] == "<<blank>>" {
                    blank_count += 1;
                } else {
                    break;
                }
            }

            assert_eq!(
                blank_count,
                1,
                "Expected exactly 1 blank line before tool block, got {}.\nAll lines:\n{}",
                blank_count,
                line_strs
                    .iter()
                    .enumerate()
                    .map(|(i, s)| format!("  [{i:2}] {s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }

        /// Test: text → tool → text → tool (interleaved) — each tool should have
        /// exactly 1 blank line before it.
        #[test]
        fn test_interleaved_text_tool_text_tool_spacing() {
            let mut renderer = create_test_harness(80, 20);
            let textarea = TextArea::new();

            // Message 1: text then tool then text then tool
            renderer.start_new_message(1);

            renderer.queue_text_delta("First paragraph.\n".to_string());
            renderer.render(&textarea);

            renderer.start_tool_use_block("read".to_string(), "t1".to_string());
            renderer.add_or_update_tool_parameter("t1", "path".to_string(), "a.txt".to_string());
            renderer.update_tool_status("t1", ToolStatus::Success, None, None);

            // More text after tool
            renderer.queue_text_delta("Second paragraph.\n".to_string());
            renderer.render(&textarea);

            renderer.start_tool_use_block("write".to_string(), "t2".to_string());
            renderer.add_or_update_tool_parameter("t2", "path".to_string(), "b.txt".to_string());
            renderer.update_tool_status("t2", ToolStatus::Success, None, None);

            // Finalize
            renderer.flush_streaming_pending();
            renderer.transcript.finalize_active_if_content();
            renderer.render(&textarea);

            let lines = renderer.drain_pending_history_lines();
            let line_strs: Vec<String> = lines
                .iter()
                .map(|l| {
                    if l.spans.is_empty() {
                        "<<blank>>".to_string()
                    } else {
                        l.spans
                            .iter()
                            .map(|s| s.content.as_ref())
                            .collect::<String>()
                    }
                })
                .collect();

            // Find all tool lines
            let tool_indices: Vec<usize> = line_strs
                .iter()
                .enumerate()
                .filter(|(_, s)| s.contains("●"))
                .map(|(i, _)| i)
                .collect();

            assert_eq!(
                tool_indices.len(),
                2,
                "Should have 2 tool lines.\nAll lines:\n{}",
                line_strs
                    .iter()
                    .enumerate()
                    .map(|(i, s)| format!("  [{i:2}] {s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );

            for &ti in &tool_indices {
                let mut blank_count = 0;
                let mut idx = ti;
                while idx > 0 {
                    idx -= 1;
                    if line_strs[idx] == "<<blank>>" {
                        blank_count += 1;
                    } else {
                        break;
                    }
                }
                assert_eq!(
                    blank_count,
                    1,
                    "Expected 1 blank before tool at line {ti}, got {blank_count}.\nAll lines:\n{}",
                    line_strs
                        .iter()
                        .enumerate()
                        .map(|(i, s)| format!("  [{i:2}] {s}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                );
            }
        }

        /// Verify trailing blank line after tool blocks in scrollback
        /// when tool is followed by more streamed content.
        #[test]
        fn test_scrollback_trailing_blank_after_tool() {
            let mut harness = create_test_harness(80, 30);
            let textarea = TextArea::new();

            harness.start_new_message(1);

            // Stream text, tool, then more text
            harness.queue_text_delta("Before tool.\n".to_string());
            harness.render(&textarea);
            let _ = harness.drain_pending_history_lines();

            harness.start_tool_use_block("read_files".to_string(), "t1".to_string());
            harness.add_or_update_tool_parameter(
                "t1",
                "paths".to_string(),
                "src/main.rs".to_string(),
            );
            harness.update_tool_status("t1", ToolStatus::Success, None, None);

            harness.queue_text_delta("After tool.\n".to_string());
            harness.render(&textarea);
            let _ = harness.drain_pending_history_lines();

            // Finalize
            harness.flush_streaming_pending();
            harness.transcript.finalize_active_if_content();
            harness.render(&textarea);

            let lines = harness.drain_pending_history_lines();
            let line_strs: Vec<String> = lines
                .iter()
                .map(|l| {
                    let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                    if text.trim().is_empty() {
                        "<<blank>>".to_string()
                    } else {
                        text
                    }
                })
                .collect();

            // Find the tool header
            let tool_idx = line_strs.iter().position(|s| s.contains("●"));
            assert!(
                tool_idx.is_some(),
                "Tool line not found.\nAll lines:\n{}",
                line_strs
                    .iter()
                    .enumerate()
                    .map(|(i, s)| format!("  [{i:2}] {s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            let ti = tool_idx.unwrap();

            // Find the last line of the tool block (before next blank or end)
            let mut last_tool_line = ti;
            #[allow(clippy::needless_range_loop)] // Index needed for last_tool_line
            for i in (ti + 1)..line_strs.len() {
                if line_strs[i] == "<<blank>>" {
                    break;
                }
                last_tool_line = i;
            }

            // There should be a blank line after the tool block
            let next = last_tool_line + 1;
            assert!(
                next < line_strs.len() && line_strs[next] == "<<blank>>",
                "Expected blank line after tool block (last tool line at {last_tool_line}).\nAll lines:\n{}",
                line_strs
                    .iter()
                    .enumerate()
                    .map(|(i, s)| format!("  [{i:2}] {s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }

        /// Compare blank lines before execute_command vs edit tool blocks.
        /// Both should have exactly 1 blank line before their header.
        #[test]
        fn test_execute_command_vs_edit_blank_lines() {
            // Helper: collect all history lines from a scenario
            fn drain_to_strings(harness: &mut TestHarness) -> Vec<String> {
                harness
                    .drain_pending_history_lines()
                    .iter()
                    .map(|l| {
                        let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                        if text.trim().is_empty() {
                            "<<blank>>".to_string()
                        } else {
                            text
                        }
                    })
                    .collect()
            }

            fn count_blanks_before(lines: &[String], ti: usize) -> usize {
                let mut count = 0;
                let mut idx = ti;
                while idx > 0 {
                    idx -= 1;
                    if lines[idx] == "<<blank>>" {
                        count += 1;
                    } else {
                        break;
                    }
                }
                count
            }

            // Scenario: text → tool (single tool)
            fn run_single_tool(tool_name: &str, tool_id: &str) -> Vec<String> {
                let mut harness = create_test_harness(80, 30);
                let textarea = TextArea::new();
                let mut all = Vec::new();

                harness.start_new_message(1);
                harness.queue_text_delta("I will now use a tool.\n".to_string());
                harness.render(&textarea);
                all.extend(drain_to_strings(&mut harness));

                harness.start_tool_use_block(tool_name.to_string(), tool_id.to_string());
                harness.render(&textarea);
                all.extend(drain_to_strings(&mut harness));

                harness.update_tool_status(
                    tool_id,
                    ToolStatus::Success,
                    Some("done".to_string()),
                    None,
                );
                harness.flush_streaming_pending();
                harness.transcript.finalize_active_if_content();
                harness.render(&textarea);
                all.extend(drain_to_strings(&mut harness));
                all
            }

            let edit_lines = run_single_tool("edit", "e1");
            let cmd_lines = run_single_tool("execute_command", "c1");

            let edit_ti = edit_lines
                .iter()
                .position(|s| s.contains("●"))
                .expect("no edit header");
            let cmd_ti = cmd_lines
                .iter()
                .position(|s| s.contains("●"))
                .expect("no cmd header");

            let edit_blanks = count_blanks_before(&edit_lines, edit_ti);
            let cmd_blanks = count_blanks_before(&cmd_lines, cmd_ti);

            assert_eq!(
                edit_blanks,
                cmd_blanks,
                "Single tool: edit blanks ({edit_blanks}) != cmd blanks ({cmd_blanks})\n\
                 Edit:\n{}\nCmd:\n{}",
                edit_lines
                    .iter()
                    .enumerate()
                    .map(|(i, s)| format!("  [{i:2}] {s}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                cmd_lines
                    .iter()
                    .enumerate()
                    .map(|(i, s)| format!("  [{i:2}] {s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );

            assert_eq!(edit_blanks, 1, "Expected 1 blank before tool");

            // Scenario: text → edit → text → execute_command (two tools with text between)
            {
                let mut harness = create_test_harness(80, 30);
                let textarea = TextArea::new();
                let mut all = Vec::new();

                harness.start_new_message(1);
                harness.queue_text_delta("First text.\n".to_string());
                harness.render(&textarea);
                all.extend(drain_to_strings(&mut harness));

                harness.start_tool_use_block("edit".to_string(), "e1".to_string());
                harness.render(&textarea);
                all.extend(drain_to_strings(&mut harness));

                harness.update_tool_status(
                    "e1",
                    ToolStatus::Success,
                    Some("done".to_string()),
                    None,
                );

                harness.queue_text_delta("Second text.\n".to_string());
                harness.render(&textarea);
                all.extend(drain_to_strings(&mut harness));

                harness.start_tool_use_block("execute_command".to_string(), "c1".to_string());
                harness.add_or_update_tool_parameter(
                    "c1",
                    "command_line".to_string(),
                    "cargo test".to_string(),
                );
                // Simulate streaming output
                harness.append_tool_output("c1", "running tests...\n");
                harness.render(&textarea);
                all.extend(drain_to_strings(&mut harness));

                harness.update_tool_status(
                    "c1",
                    ToolStatus::Success,
                    Some("done".to_string()),
                    None,
                );

                harness.flush_streaming_pending();
                harness.transcript.finalize_active_if_content();
                harness.render(&textarea);
                all.extend(drain_to_strings(&mut harness));

                // Find both tool headers
                let tool_positions: Vec<(usize, &String)> = all
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.contains("●"))
                    .collect();

                assert_eq!(
                    tool_positions.len(),
                    2,
                    "Expected 2 tool headers.\nAll lines:\n{}",
                    all.iter()
                        .enumerate()
                        .map(|(i, s)| format!("  [{i:2}] {s}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                );

                for (ti, tool_line) in &tool_positions {
                    let blanks = count_blanks_before(&all, *ti);
                    assert_eq!(
                        blanks,
                        1,
                        "Expected 1 blank before '{}' at line {ti}, got {blanks}.\nAll lines:\n{}",
                        tool_line,
                        all.iter()
                            .enumerate()
                            .map(|(i, s)| format!("  [{i:2}] {s}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                }
            }
        }

        /// Scenario: hidden tool completes → text → execute_command should still
        /// have only 1 blank before the tool header.
        #[test]
        fn test_hidden_tool_then_execute_command_blank_lines() {
            fn drain_to_strings(harness: &mut TestHarness) -> Vec<String> {
                harness
                    .drain_pending_history_lines()
                    .iter()
                    .map(|l| {
                        let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                        if text.trim().is_empty() {
                            "<<blank>>".to_string()
                        } else {
                            text
                        }
                    })
                    .collect()
            }

            fn count_blanks_before(lines: &[String], ti: usize) -> usize {
                let mut count = 0;
                let mut idx = ti;
                while idx > 0 {
                    idx -= 1;
                    if lines[idx] == "<<blank>>" {
                        count += 1;
                    } else {
                        break;
                    }
                }
                count
            }

            let mut harness = create_test_harness(80, 30);
            let textarea = TextArea::new();
            let mut all = Vec::new();

            harness.start_new_message(1);

            // Stream some initial text
            harness.queue_text_delta("Initial text.\n".to_string());
            harness.render(&textarea);
            all.extend(drain_to_strings(&mut harness));

            // Hidden tool completes
            harness.mark_hidden_tool_completed();

            // More text after hidden tool
            harness.queue_text_delta("After hidden tool.\n".to_string());
            harness.render(&textarea);
            all.extend(drain_to_strings(&mut harness));

            // Now start execute_command
            harness.start_tool_use_block("execute_command".to_string(), "c1".to_string());
            harness.add_or_update_tool_parameter(
                "c1",
                "command_line".to_string(),
                "cargo test".to_string(),
            );
            harness.render(&textarea);
            all.extend(drain_to_strings(&mut harness));

            harness.update_tool_status("c1", ToolStatus::Success, Some("done".to_string()), None);

            harness.flush_streaming_pending();
            harness.transcript.finalize_active_if_content();
            harness.render(&textarea);
            all.extend(drain_to_strings(&mut harness));

            let cmd_ti = all
                .iter()
                .position(|s| s.contains("● execute_command"))
                .expect("no execute_command header");

            let blanks = count_blanks_before(&all, cmd_ti);
            assert_eq!(
                blanks,
                1,
                "Expected 1 blank before execute_command, got {blanks}.\nAll lines:\n{}",
                all.iter()
                    .enumerate()
                    .map(|(i, s)| format!("  [{i:2}] {s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }

        /// Scenario: thinking → text → execute_command should have 1 blank before tool.
        #[test]
        fn test_thinking_text_then_execute_command_blank_lines() {
            fn drain_to_strings(harness: &mut TestHarness) -> Vec<String> {
                harness
                    .drain_pending_history_lines()
                    .iter()
                    .map(|l| {
                        let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                        if text.trim().is_empty() {
                            "<<blank>>".to_string()
                        } else {
                            text
                        }
                    })
                    .collect()
            }

            fn count_blanks_before(lines: &[String], ti: usize) -> usize {
                let mut count = 0;
                let mut idx = ti;
                while idx > 0 {
                    idx -= 1;
                    if lines[idx] == "<<blank>>" {
                        count += 1;
                    } else {
                        break;
                    }
                }
                count
            }

            let mut harness = create_test_harness(80, 30);
            let textarea = TextArea::new();
            let mut all = Vec::new();

            harness.start_new_message(1);

            // Stream thinking
            harness.queue_thinking_delta("Let me think...\n".to_string());
            harness.render(&textarea);
            all.extend(drain_to_strings(&mut harness));

            // Stream text (switches from thinking → text, flushes thinking)
            harness.queue_text_delta("I will run a command.\n".to_string());
            harness.render(&textarea);
            all.extend(drain_to_strings(&mut harness));

            // Now start execute_command
            harness.start_tool_use_block("execute_command".to_string(), "c1".to_string());
            harness.add_or_update_tool_parameter(
                "c1",
                "command_line".to_string(),
                "cargo test".to_string(),
            );
            harness.render(&textarea);
            all.extend(drain_to_strings(&mut harness));

            harness.update_tool_status("c1", ToolStatus::Success, Some("done".to_string()), None);

            harness.flush_streaming_pending();
            harness.transcript.finalize_active_if_content();
            harness.render(&textarea);
            all.extend(drain_to_strings(&mut harness));

            let cmd_ti = all
                .iter()
                .position(|s| s.contains("● execute_command"))
                .expect("no execute_command header");

            let blanks = count_blanks_before(&all, cmd_ti);
            assert_eq!(
                blanks,
                1,
                "Expected 1 blank before execute_command, got {blanks}.\nAll lines:\n{}",
                all.iter()
                    .enumerate()
                    .map(|(i, s)| format!("  [{i:2}] {s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }

        /// Scenario: edit → text → execute_command, checking both tools.
        /// Also checks with streaming tool output and full render() output replacement.
        #[test]
        fn test_edit_then_text_then_execute_command_with_output() {
            fn drain_to_strings(harness: &mut TestHarness) -> Vec<String> {
                harness
                    .drain_pending_history_lines()
                    .iter()
                    .map(|l| {
                        let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                        if text.trim().is_empty() {
                            "<<blank>>".to_string()
                        } else {
                            text
                        }
                    })
                    .collect()
            }

            fn count_blanks_before(lines: &[String], ti: usize) -> usize {
                let mut count = 0;
                let mut idx = ti;
                while idx > 0 {
                    idx -= 1;
                    if lines[idx] == "<<blank>>" {
                        count += 1;
                    } else {
                        break;
                    }
                }
                count
            }

            let mut harness = create_test_harness(80, 30);
            let textarea = TextArea::new();
            let mut all = Vec::new();

            harness.start_new_message(1);

            // Stream text
            harness.queue_text_delta("Let me edit and then run.\n".to_string());
            harness.render(&textarea);
            all.extend(drain_to_strings(&mut harness));

            // Start edit tool
            harness.start_tool_use_block("edit".to_string(), "e1".to_string());
            harness.add_or_update_tool_parameter(
                "e1",
                "file_path".to_string(),
                "src/main.rs".to_string(),
            );
            harness.update_tool_status("e1", ToolStatus::Success, Some("done".to_string()), None);
            harness.render(&textarea);
            all.extend(drain_to_strings(&mut harness));

            // More text between tools
            harness.queue_text_delta("Now running the command.\n".to_string());
            harness.render(&textarea);
            all.extend(drain_to_strings(&mut harness));

            // Start execute_command
            harness.start_tool_use_block("execute_command".to_string(), "c1".to_string());
            harness.add_or_update_tool_parameter(
                "c1",
                "command_line".to_string(),
                "cargo test".to_string(),
            );
            // Streaming output
            harness.append_tool_output("c1", "test result: ok\n");
            harness.render(&textarea);
            all.extend(drain_to_strings(&mut harness));

            // Complete with render() output (replaces streaming output)
            harness.update_tool_status(
                "c1",
                ToolStatus::Success,
                Some("Command executed successfully".to_string()),
                Some(
                    "Status: Success\n>>>>> OUTPUT:\ntest result: ok\n<<<<< END OF OUTPUT"
                        .to_string(),
                ),
            );

            harness.flush_streaming_pending();
            harness.transcript.finalize_active_if_content();
            harness.render(&textarea);
            all.extend(drain_to_strings(&mut harness));

            // Check both tools
            let tool_positions: Vec<(usize, String)> = all
                .iter()
                .enumerate()
                .filter(|(_, s)| s.contains("●"))
                .map(|(i, s)| (i, s.clone()))
                .collect();

            assert_eq!(
                tool_positions.len(),
                2,
                "Expected 2 tool headers.\nAll lines:\n{}",
                all.iter()
                    .enumerate()
                    .map(|(i, s)| format!("  [{i:2}] {s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );

            for (ti, tool_line) in &tool_positions {
                let blanks = count_blanks_before(&all, *ti);
                assert_eq!(
                    blanks,
                    1,
                    "Expected 1 blank before '{}' at line {ti}, got {blanks}.\nAll lines:\n{}",
                    tool_line,
                    all.iter()
                        .enumerate()
                        .map(|(i, s)| format!("  [{i:2}] {s}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                );
            }
        }
    }
}
