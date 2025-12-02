use crate::ui::{async_trait, DisplayFragment, UIError, UiEvent, UserInterface};
use std::any::Any;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::{watch, Mutex};
use tracing::{debug, trace, warn};

use super::renderer::ProductionTerminalRenderer;
use super::state::AppState;

#[derive(Clone)]
pub struct TerminalTuiUI {
    app_state: Arc<Mutex<AppState>>,
    redraw_tx: Arc<Mutex<Option<watch::Sender<()>>>>,
    pub cancel_flag: Arc<AtomicBool>,
    pub renderer: Arc<Mutex<Option<Arc<Mutex<ProductionTerminalRenderer>>>>>,
    event_sender: Arc<Mutex<Option<async_channel::Sender<UiEvent>>>>,
}

impl TerminalTuiUI {
    pub fn new() -> Self {
        Self {
            app_state: Arc::new(Mutex::new(AppState::new())),
            redraw_tx: Arc::new(Mutex::new(None)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            renderer: Arc::new(Mutex::new(None)),
            event_sender: Arc::new(Mutex::new(None)),
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
    pub async fn set_event_sender(&self, sender: async_channel::Sender<UiEvent>) {
        *self.event_sender.lock().await = Some(sender);
    }

    /// Helper to push an event to the queue
    fn push_event(&self, event: UiEvent) {
        let rt = tokio::runtime::Handle::current();
        let event_sender = self.event_sender.clone();
        rt.spawn(async move {
            if let Some(sender) = event_sender.lock().await.as_ref() {
                if let Err(err) = sender.send(event).await {
                    warn!("Failed to send event via channel: {}", err);
                }
            }
        });
    }
}

#[async_trait]
impl UserInterface for TerminalTuiUI {
    async fn send_event(&self, event: UiEvent) -> Result<(), UIError> {
        let mut state = self.app_state.lock().await;

        match event {
            UiEvent::SetMessages {
                messages: _,
                session_id,
                tool_results,
            } => {
                debug!("Setting messages for session {:?}", session_id);

                if let Some(session_id) = session_id {
                    if state.current_session_id.as_ref() != Some(&session_id) {
                        state.set_plan(None);
                    }
                    state.current_session_id = Some(session_id);
                }

                // Update tool statuses from tool results
                for tool_result in tool_results {
                    state
                        .tool_statuses
                        .insert(tool_result.tool_id, tool_result.status);
                }
            }

            UiEvent::UpdateMemory { memory: _ } => {
                // Memory UI has been removed - this event is ignored
            }
            UiEvent::UpdatePlan { plan } => {
                debug!("Updating plan");
                let plan_clone = plan.clone();
                state.set_plan(Some(plan));

                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.set_plan_state(Some(plan_clone));
                    renderer_guard.set_plan_expanded(state.plan_expanded);
                }
            }
            UiEvent::UpdateChatList { sessions } => {
                debug!("Updating chat list with {} sessions", sessions.len());
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
                state.update_session_activity_state(session_id.clone(), activity_state.clone());
                let is_idle = matches!(
                    &activity_state,
                    crate::session::instance::SessionActivityState::Idle
                );
                if let Some(current_session_id) = &state.current_session_id {
                    if current_session_id == &session_id {
                        state.update_activity_state(Some(activity_state));
                        if is_idle {
                            self.cancel_flag.store(false, Ordering::SeqCst);
                        }
                    }
                }
            }
            UiEvent::UpdatePendingMessage { message } => {
                debug!("Updating pending message: {:?}", message);
                state.update_pending_message(message.clone());

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
            } => {
                debug!("Updating tool status for {}: {:?}", tool_id, status);
                state.tool_statuses.insert(tool_id.clone(), status);

                // Update tool status in renderer - can now update any tool in current message
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.update_tool_status(&tool_id, status, message, output);
                }
            }
            UiEvent::ClearMessages => {
                debug!("Clearing messages");
                // Clear all messages in renderer
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.clear_all_messages();
                }
            }
            UiEvent::DisplayUserInput {
                content,
                attachments,
            } => {
                debug!("Displaying user input: {}", content);

                // Add user message
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    // Clear any existing error when user sends a message
                    renderer_guard.clear_error();
                    let formatted = format!("\n\n**User:** {content}\n");
                    let _ = renderer_guard.add_user_message(&formatted);

                    for attachment in &attachments {
                        match attachment {
                            crate::persistence::DraftAttachment::Text { content } => {
                                let attachment_text = format!("  [attachment: text]\n{content}\n");
                                let _ = renderer_guard.add_user_message(&attachment_text);
                            }
                            crate::persistence::DraftAttachment::Image { mime_type, .. } => {
                                let attachment_text =
                                    format!("  [attachment: image ({mime_type})]\n");
                                let _ = renderer_guard.add_user_message(&attachment_text);
                            }
                            crate::persistence::DraftAttachment::File { filename, .. } => {
                                let attachment_text =
                                    format!("  [attachment: file ({filename})]\n");
                                let _ = renderer_guard.add_user_message(&attachment_text);
                            }
                        }
                    }
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
            UiEvent::StreamingStarted(request_id) => {
                debug!("Streaming started for request {}", request_id);
                self.cancel_flag.store(false, Ordering::SeqCst);
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
                    renderer_guard.ensure_last_block_type(super::message::MessageBlock::PlainText(
                        super::message::PlainTextBlock::new(),
                    ));
                    renderer_guard.append_to_live_block(&content);
                }
            }
            UiEvent::AppendToThinkingBlock { content } => {
                debug!("Appending to thinking block: '{content}'");

                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.ensure_last_block_type(super::message::MessageBlock::Thinking(
                        super::message::ThinkingBlock::new(),
                    ));

                    if !content.trim().is_empty() {
                        renderer_guard.append_to_live_block(&content);
                    }
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
            } => {
                debug!("Updating tool parameter: {name} = '{value}'");

                // Update parameter in current message
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.add_or_update_tool_parameter(&tool_id, name, value);
                }
            }
            UiEvent::EndTool { id: _ } => {
                // EndTool just marks the end of parameter streaming
                // The actual status comes later via UpdateToolStatus
                // For now, we don't change the status here - wait for UpdateToolStatus
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

                // Don't finalize the message yet - keep it live for tool status updates
                // It will be finalized when the next StreamingStarted event arrives
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

    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError> {
        // Hide spinner when first content arrives
        let rt = tokio::runtime::Handle::current();
        let renderer = self.renderer.clone();
        rt.spawn(async move {
            if let Some(renderer) = renderer.lock().await.as_ref() {
                let mut renderer_guard = renderer.lock().await;
                renderer_guard.hide_loading_spinner_if_active();
            }
        });

        // Convert display fragments to UI events using push_event (like GPUI)
        match fragment {
            DisplayFragment::PlainText(text) => {
                self.push_event(UiEvent::AppendToTextBlock {
                    content: text.clone(),
                });
            }
            DisplayFragment::ThinkingText(text) => {
                self.push_event(UiEvent::AppendToThinkingBlock {
                    content: text.clone(),
                });
            }
            DisplayFragment::ToolName { name, id } => {
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

                // For terminal UI, we can append the streaming output to the tool
                // For now, just log it - we'll implement proper streaming display later
                trace!("Tool {} streaming output: {}", tool_id, chunk);
            }
            DisplayFragment::ToolTerminal {
                tool_id,
                terminal_id,
            } => {
                debug!(
                    "Tool {tool_id} attached client terminal {terminal_id}; terminal UI has no live view"
                );
            }
            DisplayFragment::CompactionDivider { summary } => {
                self.push_event(UiEvent::DisplayCompactionSummary {
                    summary: summary.clone(),
                });
            }
            DisplayFragment::ReasoningComplete => {
                // For terminal UI, no specific action needed for reasoning completion
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
                renderer_guard.hide_spinner();
            }
        });
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
