use crate::ui::{async_trait, DisplayFragment, UIError, UiEvent, UserInterface};
use std::any::Any;
use std::sync::Arc;
use tokio::sync::{watch, Mutex};
use tracing::debug;

use super::renderer::TerminalRenderer;
use super::state::AppState;

#[derive(Clone)]
pub struct TerminalTuiUI {
    app_state: Arc<Mutex<AppState>>,
    redraw_tx: Arc<Mutex<Option<watch::Sender<()>>>>,
    pub cancel_flag: Arc<Mutex<bool>>,
    pub renderer: Arc<Mutex<Option<Arc<Mutex<TerminalRenderer>>>>>,
}

impl TerminalTuiUI {
    pub fn new() -> Self {
        Self {
            app_state: Arc::new(Mutex::new(AppState::new())),
            redraw_tx: Arc::new(Mutex::new(None)),
            cancel_flag: Arc::new(Mutex::new(false)),
            renderer: Arc::new(Mutex::new(None)),
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

    pub async fn set_renderer_async(&self, renderer: Arc<Mutex<TerminalRenderer>>) {
        *self.renderer.lock().await = Some(renderer);
    }

    /// Trigger a redraw
    async fn trigger_redraw(&self) {
        if let Some(tx) = self.redraw_tx.lock().await.as_ref() {
            let _ = tx.send(());
        }
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
                    state.current_session_id = Some(session_id);
                }

                // Update tool statuses from tool results
                for tool_result in tool_results {
                    state
                        .tool_statuses
                        .insert(tool_result.tool_id, tool_result.status);
                }
            }
            UiEvent::UpdateMemory { memory } => {
                debug!("Updating memory");
                state.working_memory = Some(memory);
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
                if let Some(current_session_id) = &state.current_session_id {
                    if current_session_id == &session_id {
                        state.update_activity_state(Some(activity_state));
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
            UiEvent::StreamingStarted(_request_id) => {
                debug!("Streaming started");
                // Start a new message - this will finalize any existing live message
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.start_new_message();
                }
            }
            UiEvent::AppendToTextBlock { content } => {
                debug!("Appending to text block: '{content}'");

                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;

                    // Ensure we have a live message
                    if renderer_guard.live_message.is_none() {
                        renderer_guard.start_new_message();
                    }

                    // Check if we need a new plain text block
                    let needs_new_text_block = if let Some(ref message) = renderer_guard.live_message {
                        match message.blocks.last() {
                            Some(super::message::MessageBlock::PlainText(_)) => false,
                            Some(_) => true, // Different block type, need new block
                            None => true,    // No blocks, need new block
                        }
                    } else {
                        true
                    };

                    if needs_new_text_block {
                        renderer_guard.start_plain_text_block();
                    }

                    renderer_guard.append_to_live_block(&content);
                }
            }
            UiEvent::AppendToThinkingBlock { content } => {
                debug!("Appending to thinking block: '{content}'");

                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;

                    // Ensure we have a live message
                    if renderer_guard.live_message.is_none() {
                        renderer_guard.start_new_message();
                    }

                    // Check if we need a new thinking block
                    let needs_new_thinking_block = if let Some(ref message) = renderer_guard.live_message {
                        match message.blocks.last() {
                            Some(super::message::MessageBlock::Thinking(_)) => false,
                            Some(_) => true, // Different block type, need new block
                            None => true,    // No blocks, need new block
                        }
                    } else {
                        true
                    };

                    if needs_new_thinking_block {
                        renderer_guard.start_thinking_block();
                    }

                    if !content.trim().is_empty() {
                        renderer_guard.append_to_live_block(&content);
                    }
                }
            }
            UiEvent::StartTool { name, id } => {
                debug!("Starting tool: {} ({})", name, id);

                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;

                    // Ensure we have a live message
                    if renderer_guard.live_message.is_none() {
                        renderer_guard.start_new_message();
                    }

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
            UiEvent::StreamingStopped { id, cancelled } => {
                debug!("Streaming stopped (id: {}, cancelled: {})", id, cancelled);

                // Don't finalize the message yet - keep it live for tool status updates
                // It will be finalized when the next StreamingStarted event arrives
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
        // Convert display fragments to UI events that can be processed asynchronously
        // This avoids blocking in the sync display_fragment method
        let rt = tokio::runtime::Handle::current();
        let self_clone = self.clone();

        match fragment {
            DisplayFragment::PlainText(text) => {
                let text_clone = text.clone();
                rt.spawn(async move {
                    let _ = self_clone
                        .send_event(UiEvent::AppendToTextBlock {
                            content: text_clone,
                        })
                        .await;
                });
            }
            DisplayFragment::ThinkingText(text) => {
                let text_clone = text.clone();
                rt.spawn(async move {
                    let _ = self_clone
                        .send_event(UiEvent::AppendToThinkingBlock {
                            content: text_clone,
                        })
                        .await;
                });
            }
            DisplayFragment::ToolName { name, id } => {
                let name_clone = name.clone();
                let id_clone = id.clone();
                rt.spawn(async move {
                    let _ = self_clone
                        .send_event(UiEvent::StartTool {
                            name: name_clone,
                            id: id_clone,
                        })
                        .await;
                });
            }
            DisplayFragment::ToolParameter {
                name,
                value,
                tool_id,
            } => {
                let name_clone = name.clone();
                let value_clone = value.clone();
                let tool_id_clone = tool_id.clone();
                rt.spawn(async move {
                    let _ = self_clone
                        .send_event(UiEvent::UpdateToolParameter {
                            tool_id: tool_id_clone,
                            name: name_clone,
                            value: value_clone,
                        })
                        .await;
                });
            }
            DisplayFragment::ToolEnd { id } => {
                let id_clone = id.clone();
                rt.spawn(async move {
                    let _ = self_clone
                        .send_event(UiEvent::EndTool { id: id_clone })
                        .await;
                });
            }
            DisplayFragment::Image { media_type, data } => {
                let media_type_clone = media_type.clone();
                let data_clone = data.clone();
                rt.spawn(async move {
                    let _ = self_clone
                        .send_event(UiEvent::AddImage {
                            media_type: media_type_clone,
                            data: data_clone,
                        })
                        .await;
                });
            }
        }
        Ok(())
    }

    fn should_streaming_continue(&self) -> bool {
        // Check cancel flag
        if let Ok(cancel_flag) = self.cancel_flag.try_lock() {
            !*cancel_flag
        } else {
            true // If we can't get the lock, assume we should continue
        }
    }

    fn notify_rate_limit(&self, seconds_remaining: u64) {
        debug!("Rate limited for {} seconds", seconds_remaining);
        // Could add rate limit notification to renderer here
    }

    fn clear_rate_limit(&self) {
        debug!("Rate limit cleared");
        // Could clear rate limit notification from renderer here
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
