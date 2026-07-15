use async_trait::async_trait;
use code_assistant_core::ui::{DisplayFragment, UIError, UiEvent, UserInterface};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::{watch, Mutex};
use tracing::{debug, warn};

use super::message::{LiveMessage, MessageBlock, PlainTextBlock, ThinkingBlock, ToolUseBlock};
use super::renderer::ProductionTerminalRenderer;
use super::state::AppState;

#[derive(Clone)]
pub struct TerminalUI {
    app_state: Arc<Mutex<AppState>>,
    redraw_tx: Arc<Mutex<Option<watch::Sender<()>>>>,
    pub cancel_flag: Arc<AtomicBool>,
    pub renderer: Arc<Mutex<Option<Arc<Mutex<ProductionTerminalRenderer>>>>>,
    event_sender: Arc<std::sync::Mutex<Option<async_channel::Sender<UiEvent>>>>,
}

impl TerminalUI {
    pub fn new_with_state(app_state: Arc<Mutex<AppState>>) -> Self {
        Self {
            app_state,
            redraw_tx: Arc::new(Mutex::new(None)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            renderer: Arc::new(Mutex::new(None)),
            event_sender: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    #[allow(dead_code)]
    pub fn get_app_state(&self) -> Arc<Mutex<AppState>> {
        self.app_state.clone()
    }

    pub fn set_redraw_sender(&self, tx: watch::Sender<()>) {
        let redraw_tx = self.redraw_tx.clone();
        let rt = tokio::runtime::Handle::current();
        rt.spawn(async move {
            *redraw_tx.lock().await = Some(tx);
        });
    }

    pub async fn set_renderer_async(&self, renderer: Arc<Mutex<ProductionTerminalRenderer>>) {
        *self.renderer.lock().await = Some(renderer);
    }

    /// Trigger a redraw
    async fn trigger_redraw(&self) {
        if let Some(tx) = self.redraw_tx.lock().await.as_ref() {
            let _ = tx.send(());
        }
    }

    /// Set the event sender for pushing events
    pub fn set_event_sender(&self, sender: async_channel::Sender<UiEvent>) {
        *self
            .event_sender
            .lock()
            .expect("event_sender lock poisoned") = Some(sender);
    }

    /// Helper to push an event to the queue.
    /// Uses synchronous `try_send` on an unbounded channel to guarantee FIFO
    /// ordering.  The previous implementation spawned a Tokio task per event,
    /// which could reorder events when two tasks raced for the async mutex.
    fn push_event(&self, event: UiEvent) {
        let guard = self
            .event_sender
            .lock()
            .expect("event_sender lock poisoned");
        if let Some(sender) = guard.as_ref() {
            if let Err(err) = sender.try_send(event) {
                warn!("Failed to send event via channel: {}", err);
            }
        }
    }

    /// Render a complete message (appended by another code-assistant
    /// instance) into the scrollback, mapping its fragments onto the same
    /// renderer calls the live-streaming path uses.
    /// Render a complete message (from a session snapshot replay or a watcher
    /// append) directly into the transcript as a committed message.
    ///
    /// These messages arrive complete, so we do NOT simulate live streaming:
    /// the streaming path flushes text/thinking to scrollback immediately but
    /// defers tool/user blocks to the next redraw, which scrambles block and
    /// message order during a replay (no redraws happen mid-replay) and leaves
    /// the final message stuck in a live/spinner state. Building committed
    /// messages preserves order and lets `prepare()` flush them cleanly.
    fn render_message_data(
        &self,
        renderer: &mut ProductionTerminalRenderer,
        message: code_assistant_core::ui::ui_events::MessageData,
    ) {
        use code_assistant_core::ui::ui_events::MessageRole;

        let mut live = LiveMessage {
            finalized: true,
            ..LiveMessage::default()
        };

        match message.role {
            MessageRole::User => {
                let mut block = PlainTextBlock::new();
                block.content = plain_text_of(&message.fragments);
                live.add_block(MessageBlock::UserText(block));
            }
            MessageRole::System => {
                let mut block = PlainTextBlock::new();
                block.content = plain_text_of(&message.fragments);
                live.add_block(MessageBlock::PlainText(block));
            }
            MessageRole::Assistant => {
                for fragment in message.fragments {
                    match fragment {
                        DisplayFragment::PlainText(text) => {
                            append_text_block(&mut live, &text);
                        }
                        DisplayFragment::ThinkingText { text, .. }
                        | DisplayFragment::ReasoningSummaryDelta(text) => {
                            append_thinking_block(&mut live, &text);
                        }
                        DisplayFragment::ToolName { name, id, .. } => {
                            live.add_block(MessageBlock::ToolUse(ToolUseBlock::new(name, id)));
                        }
                        DisplayFragment::ToolParameter {
                            name,
                            value,
                            tool_id,
                        } => {
                            if let Some(tool) = live.get_tool_block_mut(&tool_id) {
                                tool.add_or_update_parameter(name, value);
                            }
                        }
                        DisplayFragment::ToolOutput { tool_id, chunk } => {
                            if let Some(tool) = live.get_tool_block_mut(&tool_id) {
                                tool.output.get_or_insert_with(String::new).push_str(&chunk);
                            }
                        }
                        DisplayFragment::CompactionDivider { summary } => {
                            append_text_block(
                                &mut live,
                                &format!("\n\n[conversation compacted]\n{summary}\n"),
                            );
                        }
                        DisplayFragment::HiddenToolCompleted => {
                            // Preserve a paragraph break where a hidden tool sat
                            // between two prose fragments.
                            if let Some(MessageBlock::PlainText(block)) = live.get_last_block_mut()
                            {
                                block.content.push_str("\n\n");
                            }
                        }
                        DisplayFragment::Image { media_type, .. } => {
                            append_text_block(&mut live, &format!("[image ({media_type})]"));
                        }
                        DisplayFragment::ToolEnd { .. }
                        | DisplayFragment::ToolTerminal { .. }
                        | DisplayFragment::ToolTerminalOutput { .. }
                        | DisplayFragment::ToolTerminalExited { .. }
                        | DisplayFragment::ReasoningSummaryStart
                        | DisplayFragment::ReasoningComplete => {}
                    }
                }
            }
        }

        if live.has_content() {
            renderer.push_complete_message(live);
        }
    }
}

/// Append plain text to the message, extending the trailing PlainText block if
/// there is one, otherwise starting a new block.
fn append_text_block(message: &mut LiveMessage, text: &str) {
    if let Some(MessageBlock::PlainText(block)) = message.get_last_block_mut() {
        block.content.push_str(text);
    } else {
        let mut block = PlainTextBlock::new();
        block.content.push_str(text);
        message.add_block(MessageBlock::PlainText(block));
    }
}

/// Append thinking text, extending the trailing Thinking block if there is one.
fn append_thinking_block(message: &mut LiveMessage, text: &str) {
    if let Some(MessageBlock::Thinking(block)) = message.get_last_block_mut() {
        block.content.push_str(text);
    } else {
        let mut block = ThinkingBlock::new();
        block.content.push_str(text);
        message.add_block(MessageBlock::Thinking(block));
    }
}

/// Concatenate the plain-text fragments of a message (user/system messages
/// carry their content as plain text).
fn plain_text_of(fragments: &[DisplayFragment]) -> String {
    let mut text = String::new();
    for fragment in fragments {
        if let DisplayFragment::PlainText(part) = fragment {
            text.push_str(part);
        }
    }
    text
}

impl TerminalUI {
    /// Apply a single event to the app state and the renderer.
    ///
    /// Only the task draining the event queue calls this: every producer
    /// enqueues via [`Self::send_event`] / [`Self::display_fragment`] so that
    /// events are applied in emission order.
    pub async fn handle_event(&self, event: UiEvent) -> Result<(), UIError> {
        match event {
            UiEvent::SetMessages {
                messages,
                session_id,
                tool_results,
            } => {
                let mut state = self.app_state.lock().await;
                debug!("Setting messages for session {:?}", session_id);

                if let Some(session_id) = session_id {
                    if state.current_session_id.as_ref() != Some(&session_id) {
                        state.set_plan(None);
                    }
                    state.current_session_id = Some(session_id);
                }

                // The terminal doesn't replay history into the scrollback,
                // but it must know which nodes the transcript baseline covers
                // so later watcher appends only render genuinely new content.
                state.reset_seen_nodes(messages.iter().filter_map(|m| m.node_id));

                // Update tool statuses from tool results
                for tool_result in tool_results {
                    state
                        .tool_statuses
                        .insert(tool_result.tool_id, tool_result.status);
                }
            }

            UiEvent::AppendMessages {
                messages,
                tool_results,
            } => {
                // Messages appended by another code-assistant instance (file
                // watcher). Deduplicate by node id: locally streamed content
                // carries the same pre-allocated node id, so a watcher refresh
                // racing the agent's Idle transition must not render it again.
                let fresh: Vec<_> = {
                    let mut state = self.app_state.lock().await;
                    for tool_result in &tool_results {
                        state
                            .tool_statuses
                            .insert(tool_result.tool_id.clone(), tool_result.status);
                    }
                    messages
                        .into_iter()
                        .filter(|message| match message.node_id {
                            Some(node_id) => state.mark_node_seen(node_id),
                            None => true,
                        })
                        .collect()
                };
                debug!("Appending {} externally added message(s)", fresh.len());

                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    // Build committed messages first, then apply tool results to
                    // them so they carry the correct status/output when the next
                    // redraw flushes them to scrollback in order.
                    for message in fresh {
                        self.render_message_data(&mut renderer_guard, message);
                    }
                    for tool_result in tool_results {
                        renderer_guard.update_tool_status(
                            &tool_result.tool_id,
                            tool_result.status,
                            tool_result.message,
                            tool_result.output,
                        );
                    }
                }
            }

            UiEvent::UpdatePlan { plan } => {
                debug!("Updating plan");
                let plan_clone = plan.clone();
                let (plan_expanded, overlay_active) = {
                    let mut state = self.app_state.lock().await;
                    state.set_plan(Some(plan));
                    (state.plan_expanded, state.is_overlay_active())
                };

                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.set_plan_state(Some(plan_clone));
                    renderer_guard.set_plan_expanded(plan_expanded);
                    renderer_guard.set_overlay_active(overlay_active);
                }
            }
            UiEvent::UpdateChatList { sessions } => {
                debug!("Updating chat list with {} sessions", sessions.len());
                let mut state = self.app_state.lock().await;
                state.update_sessions(sessions);
            }
            UiEvent::UpdateSessionActivityState {
                session_id,
                activity_state,
            } => {
                debug!(
                    "Updating activity state for session {}: {:?}",
                    session_id, activity_state
                );
                let mut state = self.app_state.lock().await;
                state.update_session_activity_state(session_id.clone(), activity_state.clone());
                let is_terminal = activity_state.is_terminal();
                if let Some(current_session_id) = &state.current_session_id {
                    if current_session_id == &session_id {
                        state.update_activity_state(Some(activity_state));
                        if is_terminal {
                            self.cancel_flag.store(false, Ordering::SeqCst);
                        }
                    }
                }
            }
            UiEvent::UpdatePendingMessage { message } => {
                debug!("Updating pending message: {:?}", message);
                {
                    let mut state = self.app_state.lock().await;
                    state.update_pending_message(message.clone());
                }

                // Set pending message in renderer if available
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.set_pending_user_message(message);
                }
            }

            UiEvent::UpdateToolStatus {
                tool_id,
                status,
                message,
                output,
                ..
            } => {
                debug!("Updating tool status for {}: {:?}", tool_id, status);
                {
                    let mut state = self.app_state.lock().await;
                    state.tool_statuses.insert(tool_id.clone(), status);
                }

                // Update tool status in renderer - can now update any tool in current message
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.update_tool_status(&tool_id, status, message, output);
                }
            }
            UiEvent::ClearMessages => {
                debug!("Clearing messages");
                self.app_state.lock().await.reset_seen_nodes([]);
                // Clear all messages in renderer
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.clear_all_messages();
                }
            }

            UiEvent::DisplayUserInput {
                content,
                attachments,
                node_id,
            } => {
                debug!("Displaying user input: {}", content);
                if let Some(node_id) = node_id {
                    self.app_state.lock().await.mark_node_seen(node_id);
                }

                // Add user message
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    // Clear any existing error when user sends a message
                    renderer_guard.clear_error();
                    // Build combined content with attachment info merged in
                    let mut display_content = content.clone();
                    let attachment_lines: Vec<String> = attachments
                        .iter()
                        .map(|attachment| match attachment {
                            code_assistant_core::persistence::DraftAttachment::Text { .. } => {
                                "[text attachment]".to_string()
                            }
                            code_assistant_core::persistence::DraftAttachment::Image {
                                mime_type,
                                width,
                                height,
                                ..
                            } => {
                                let dims = match (width, height) {
                                    (Some(w), Some(h)) => format!("{w}x{h} "),
                                    _ => String::new(),
                                };
                                format!("[image {dims}({mime_type})]")
                            }
                            code_assistant_core::persistence::DraftAttachment::File {
                                filename,
                                ..
                            } => {
                                format!("[file ({filename})]")
                            }
                        })
                        .collect();
                    if !attachment_lines.is_empty() {
                        display_content.push('\n');
                        for line in &attachment_lines {
                            display_content.push('\n');
                            display_content.push_str(line);
                        }
                    }
                    let _ = renderer_guard.add_user_message(&display_content);
                }
            }
            UiEvent::DisplayCompactionSummary { summary } => {
                debug!("Displaying compaction summary");
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    let formatted = format!("\n\n[conversation compacted]\n{summary}\n",);
                    let _ = renderer_guard.add_instruction_message(&formatted);
                }
            }
            UiEvent::StreamingStarted {
                request_id,
                node_id,
            } => {
                debug!("Streaming started for request {}", request_id);
                self.cancel_flag.store(false, Ordering::SeqCst);
                // The streamed response will be persisted under the
                // pre-allocated node id — record it so a watcher append of
                // the persisted message is recognized as already rendered.
                self.app_state.lock().await.mark_node_seen(node_id);
                // Start a new message - this will finalize any existing live message
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.start_new_message(request_id);
                    // Clear any existing error when new operation starts
                    renderer_guard.clear_error();
                }
            }
            UiEvent::AppendToTextBlock { content } => {
                debug!("Appending to text block: '{content}'");

                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.queue_text_delta(content);
                }
            }
            UiEvent::AppendToThinkingBlock { content } => {
                debug!("Appending to thinking block: '{content}'");

                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.queue_thinking_delta(content);
                }
            }
            UiEvent::StartTool { name, id } => {
                debug!("Starting tool: {} ({})", name, id);

                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    // Always start a new tool use block
                    renderer_guard.start_tool_use_block(name, id);
                }
            }

            UiEvent::UpdateToolParameter {
                tool_id,
                name,
                value,
                replace,
            } => {
                debug!("Updating tool parameter: {name} = '{value}'");

                // Update parameter in current message
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    if replace {
                        renderer_guard.replace_tool_parameter(&tool_id, name, value);
                    } else {
                        renderer_guard.add_or_update_tool_parameter(&tool_id, name, value);
                    }
                }
            }

            UiEvent::EndTool { id: _ } => {
                // EndTool just marks the end of parameter streaming
                // The actual status comes later via UpdateToolStatus
                // For now, we don't change the status here - wait for UpdateToolStatus
            }
            UiEvent::AppendToolOutput { tool_id, chunk } => {
                // Accumulate streaming output into the tool block (used by execute_command)
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.append_tool_output(&tool_id, &chunk);
                }
            }
            UiEvent::HiddenToolCompleted => {
                // Mark that a hidden tool completed - renderer handles paragraph breaks
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.mark_hidden_tool_completed();
                }
            }
            UiEvent::StreamingStopped {
                id,
                cancelled,
                error,
            } => {
                debug!(
                    "Streaming stopped (id: {}, cancelled: {}, error: {:?})",
                    id, cancelled, error
                );

                self.cancel_flag.store(false, Ordering::SeqCst);

                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.flush_streaming_pending();
                }

                // Don't finalize the message yet - keep it live for tool status updates
                // It will be finalized when the next StreamingStarted event arrives
            }
            UiEvent::RollbackStreaming { id } => {
                debug!("Rolling back streamed content for request {}", id);
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.discard_active_message();
                }
            }
            UiEvent::DisplayError { message } => {
                debug!("Displaying error: {}", message);
                // Set error in renderer
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.set_error(message);
                }
            }

            UiEvent::ClearError => {
                debug!("Clearing error");
                // Clear error in renderer
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.clear_error();
                }
            }
            UiEvent::UpdatePermissionTier { tier } => {
                let mut state = self.app_state.lock().await;
                state.update_permission_tier(Some(tier));
            }
            UiEvent::RequestToolPermission { request } => {
                let mut state = self.app_state.lock().await;
                state.push_permission_request(request);
                state.open_next_permission_prompt();
            }
            UiEvent::ToolPermissionRequestResolved { request_id } => {
                let mut state = self.app_state.lock().await;
                state.remove_permission_request(&request_id);
                state.popup_stack.remove_permission_popup(&request_id);
                state.open_next_permission_prompt();
            }
            UiEvent::ShowTransientStatus { message } => {
                debug!("Transient status: {}", message);
                // In the terminal UI, show as a brief info message via the error strip
                // (it will be replaced by the next StreamingStarted)
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.set_error(message);
                }
            }
            UiEvent::ClearTransientStatus => {
                // Clear the transient status (auto-dismiss timer fired)
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.clear_error();
                }
            }
            // Resource events - logged for debugging, can be extended for features like "follow mode"
            UiEvent::ResourceLoaded { project, path } => {
                tracing::trace!(
                    "ResourceLoaded event - project: {}, path: {}",
                    project,
                    path.display()
                );
            }
            UiEvent::ResourceWritten { project, path } => {
                tracing::trace!(
                    "ResourceWritten event - project: {}, path: {}",
                    project,
                    path.display()
                );
            }
            UiEvent::DirectoryListed { project, path } => {
                tracing::trace!(
                    "DirectoryListed event - project: {}, path: {}",
                    project,
                    path.display()
                );
            }
            UiEvent::ResourceDeleted { project, path } => {
                tracing::trace!(
                    "ResourceDeleted event - project: {}, path: {}",
                    project,
                    path.display()
                );
            }
            _ => {
                // For other events, just log them
                debug!("Unhandled event: {:?}", event);
            }
        }

        // Trigger redraw after processing event
        self.trigger_redraw().await;

        Ok(())
    }
}

#[async_trait]
impl UserInterface for TerminalUI {
    /// Enqueue an event for the drain task instead of applying it here.
    ///
    /// Fragments can only reach the renderer through the queue (they arrive on
    /// a sync call that cannot take the async renderer lock), so applying
    /// events inline would let a lifecycle event overtake fragments emitted
    /// before it — `StreamingStopped` closing the stream while the tail deltas
    /// are still queued, which drops them.
    async fn send_event(&self, event: UiEvent) -> Result<(), UIError> {
        self.push_event(event);
        Ok(())
    }

    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError> {
        // Convert display fragments to UI events using push_event (like GPUI)
        match fragment {
            DisplayFragment::PlainText(text) => {
                self.push_event(UiEvent::AppendToTextBlock {
                    content: text.clone(),
                });
            }

            DisplayFragment::ThinkingText { ref text, .. } => {
                self.push_event(UiEvent::AppendToThinkingBlock {
                    content: text.clone(),
                });
            }

            DisplayFragment::ToolName { name, id, .. } => {
                if id.is_empty() {
                    warn!(
                        "StreamingProcessor provided empty tool ID for tool '{}' - this is a bug!",
                        name
                    );
                    return Err(UIError::IOError(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Empty tool ID for tool '{name}'"),
                    )));
                }

                self.push_event(UiEvent::StartTool {
                    name: name.clone(),
                    id: id.clone(),
                });
            }
            DisplayFragment::ToolParameter {
                name,
                value,
                tool_id,
            } => {
                if tool_id.is_empty() {
                    warn!("StreamingProcessor provided empty tool ID for parameter '{}' - this is a bug!", name);
                    return Err(UIError::IOError(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Empty tool ID for parameter '{name}'"),
                    )));
                }

                self.push_event(UiEvent::UpdateToolParameter {
                    tool_id: tool_id.clone(),
                    name: name.clone(),
                    value: value.clone(),
                    replace: false,
                });
            }
            DisplayFragment::ToolEnd { id } => {
                if id.is_empty() {
                    warn!("StreamingProcessor provided empty tool ID for ToolEnd - this is a bug!");
                    return Err(UIError::IOError(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Empty tool ID for ToolEnd".to_string(),
                    )));
                }

                self.push_event(UiEvent::EndTool { id: id.clone() });
            }
            DisplayFragment::Image { media_type, data } => {
                self.push_event(UiEvent::AddImage {
                    media_type: media_type.clone(),
                    data: data.clone(),
                });
            }
            DisplayFragment::ReasoningSummaryStart => {
                // Terminal UI currently treats reasoning summaries as text; no separate handling needed
            }
            DisplayFragment::ReasoningSummaryDelta(delta) => {
                // For terminal UI, treat reasoning summary as thinking text
                self.push_event(UiEvent::AppendToThinkingBlock {
                    content: delta.clone(),
                });
            }
            DisplayFragment::ToolOutput { tool_id, chunk } => {
                if tool_id.is_empty() {
                    warn!(
                        "StreamingProcessor provided empty tool ID for ToolOutput - this is a bug!"
                    );
                    return Err(UIError::IOError(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Empty tool ID for ToolOutput".to_string(),
                    )));
                }

                // Accumulate streaming output into the tool block (for execute_command display)
                self.push_event(UiEvent::AppendToolOutput {
                    tool_id: tool_id.clone(),
                    chunk: chunk.clone(),
                });
            }
            DisplayFragment::ToolTerminal {
                tool_id,
                terminal_id,
            } => {
                debug!(
                    "Tool {tool_id} attached client terminal {terminal_id}; terminal UI has no live view"
                );
            }
            DisplayFragment::ToolTerminalOutput { .. } => {
                // Raw ANSI bytes are for frontends with a terminal
                // emulator; the TUI renders the plain ToolOutput chunks.
            }
            DisplayFragment::ToolTerminalExited { .. } => {
                // Terminal exit is for frontends with a display-only
                // terminal card; the TUI has no live terminal view.
            }
            DisplayFragment::CompactionDivider { summary } => {
                self.push_event(UiEvent::DisplayCompactionSummary {
                    summary: summary.clone(),
                });
            }

            DisplayFragment::ReasoningComplete => {
                // For terminal UI, no specific action needed for reasoning completion
            }
            DisplayFragment::HiddenToolCompleted => {
                // Signal that a hidden tool completed - renderer handles paragraph breaks
                self.push_event(UiEvent::HiddenToolCompleted);
            }
        }

        Ok(())
    }

    fn should_streaming_continue(&self) -> bool {
        !self.cancel_flag.load(Ordering::SeqCst)
    }

    fn notify_rate_limit(&self, seconds_remaining: u64) {
        debug!("Rate limited for {} seconds", seconds_remaining);

        let rt = tokio::runtime::Handle::current();
        let renderer = self.renderer.clone();
        rt.spawn(async move {
            if let Some(renderer) = renderer.lock().await.as_ref() {
                let mut renderer_guard = renderer.lock().await;
                renderer_guard.show_rate_limit_spinner(seconds_remaining);
            }
        });
    }

    fn clear_rate_limit(&self) {
        debug!("Rate limit cleared");

        let rt = tokio::runtime::Handle::current();
        let renderer = self.renderer.clone();
        rt.spawn(async move {
            if let Some(renderer) = renderer.lock().await.as_ref() {
                let mut renderer_guard = renderer.lock().await;
                renderer_guard.hide_rate_limit_spinner_if_active();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_assistant_core::ui::DisplayFragment;

    /// Wire a `TerminalUI` to a renderer and an event queue, mirroring how
    /// `app.rs` assembles them.
    async fn harness() -> (
        TerminalUI,
        async_channel::Receiver<UiEvent>,
        Arc<Mutex<ProductionTerminalRenderer>>,
    ) {
        let ui = TerminalUI::new_with_state(Arc::new(Mutex::new(AppState::new())));
        let (tx, rx) = async_channel::unbounded();
        ui.set_event_sender(tx);
        let renderer = Arc::new(Mutex::new(
            ProductionTerminalRenderer::new().expect("renderer"),
        ));
        ui.set_renderer_async(renderer.clone()).await;
        (ui, rx, renderer)
    }

    /// Apply everything queued, in order, the way the app's drain task does.
    async fn drain(ui: &TerminalUI, rx: &async_channel::Receiver<UiEvent>) {
        while let Ok(event) = rx.try_recv() {
            ui.handle_event(event).await.expect("handle event");
        }
    }

    fn history_text(lines: &[ratatui::text::Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// `StreamingStopped` closes the stream, and the renderer drops any delta
    /// that arrives after it. Fragments can only reach the renderer through the
    /// queue, so a stop applied ahead of the queue would cut the tail off the
    /// final message. Every producer must go through the same queue.
    #[tokio::test]
    async fn streaming_stop_does_not_overtake_queued_fragments() {
        let (ui, rx, renderer) = harness().await;

        // Emission order, as the agent produces it: start, text…, stop.
        ui.send_event(UiEvent::StreamingStarted {
            request_id: 1,
            node_id: 0,
        })
        .await
        .expect("start");
        ui.display_fragment(&DisplayFragment::PlainText("hello ".to_string()))
            .expect("fragment");
        ui.display_fragment(&DisplayFragment::PlainText("world".to_string()))
            .expect("fragment");
        ui.send_event(UiEvent::StreamingStopped {
            id: 1,
            cancelled: false,
            error: None,
        })
        .await
        .expect("stop");

        drain(&ui, &rx).await;

        let mut guard = renderer.lock().await;
        let history = history_text(&guard.drain_pending_history_lines());
        assert!(
            history.contains("hello world"),
            "streamed text was dropped by an out-of-order stop; scrollback was {history:?}"
        );
    }

    /// Same ordering guarantee, seen from the spinner: a stale fragment from
    /// the previous request must not hide the spinner of the next one.
    #[tokio::test]
    async fn spinner_survives_a_stale_fragment_from_the_previous_request() {
        let (ui, rx, renderer) = harness().await;

        ui.send_event(UiEvent::StreamingStarted {
            request_id: 1,
            node_id: 0,
        })
        .await
        .expect("start");
        ui.display_fragment(&DisplayFragment::PlainText("first".to_string()))
            .expect("fragment");
        ui.send_event(UiEvent::StreamingStopped {
            id: 1,
            cancelled: false,
            error: None,
        })
        .await
        .expect("stop");
        // Next request goes out; its spinner must be showing afterwards.
        ui.send_event(UiEvent::StreamingStarted {
            request_id: 2,
            node_id: 1,
        })
        .await
        .expect("start 2");

        drain(&ui, &rx).await;

        let guard = renderer.lock().await;
        assert!(
            guard.is_loading_spinner_visible(),
            "request 1's fragment hid the spinner request 2 had just put up"
        );
    }
}
