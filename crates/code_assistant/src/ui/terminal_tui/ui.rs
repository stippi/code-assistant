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
                    if let Some(msg) = message {
                        let formatted = format!("\n\n**User:** {msg}\n");
                        renderer_guard.set_pending_user_message(formatted);
                    } else {
                        renderer_guard.set_pending_user_message("".to_string());
                    }
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

                // Update tool status in renderer - only if current block matches tool_id
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;

                    let is_correct_tool = match &renderer_guard.live_block {
                        Some(super::blocks::LiveBlockType::ToolUse(tool_block)) => {
                            tool_block.id == tool_id
                        }
                        _ => false,
                    };

                    if is_correct_tool {
                        renderer_guard.update_tool_status(&tool_id, status, message, output);
                    }
                }
            }
            UiEvent::ClearMessages => {
                debug!("Clearing messages");
                // Clear live and finalized blocks in renderer
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.live_block = None;
                    renderer_guard.finalized_blocks.clear();
                    renderer_guard.last_overflow = 0;
                }
            }
            UiEvent::DisplayUserInput {
                content,
                attachments,
            } => {
                debug!("Displaying user input: {}", content);

                // Add user message as finalized block
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
                // Finalize any existing live block before starting new stream
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;

                    // Finalize current block if any
                    if renderer_guard.live_block.is_some() {
                        let _ = renderer_guard.finalize_live_block();
                    }

                    // Don't start a new block yet - wait for first content
                }
            }
            UiEvent::AppendToTextBlock { content } => {
                debug!("Appending to text block: {}", content.trim());

                // Ensure we have a plain text block, or finalize current and start new one
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;

                    let needs_new_text_block = match &renderer_guard.live_block {
                        Some(super::blocks::LiveBlockType::PlainText(_)) => false,
                        Some(_) => true, // Different block type, need to finalize
                        None => true,    // No block, need to start
                    };

                    if needs_new_text_block {
                        // Finalize current block if any
                        if renderer_guard.live_block.is_some() {
                            let _ = renderer_guard.finalize_live_block();
                        }
                        renderer_guard.start_plain_text_block();
                    }

                    renderer_guard.append_to_live_block(&content);
                }
            }
            UiEvent::AppendToThinkingBlock { content } => {
                debug!("Appending to thinking block: {}", content.trim());

                // Ensure we have a thinking block, or finalize current and start new one
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;

                    let needs_new_thinking_block = match &renderer_guard.live_block {
                        Some(super::blocks::LiveBlockType::Thinking(_)) => false,
                        Some(_) => true, // Different block type, need to finalize
                        None => true,    // No block, need to start
                    };

                    if needs_new_thinking_block {
                        // Finalize current block if any
                        if renderer_guard.live_block.is_some() {
                            let _ = renderer_guard.finalize_live_block();
                        }
                        renderer_guard.start_thinking_block();
                    }

                    if !content.trim().is_empty() {
                        renderer_guard.append_to_live_block(&content);
                    }
                }
            }
            UiEvent::StartTool { name, id } => {
                debug!("Starting tool: {} ({})", name, id);

                // Finalize current block if any, then start new tool use block
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;

                    // Always finalize current block when starting a new tool
                    if renderer_guard.live_block.is_some() {
                        let _ = renderer_guard.finalize_live_block();
                    }

                    renderer_guard.start_tool_use_block(name, id);
                }
            }
            UiEvent::UpdateToolParameter {
                tool_id,
                name,
                value,
            } => {
                debug!("Updating tool parameter: {} = {}", name, value.trim());

                // Update parameter in tool use block - only if current block matches tool_id
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;

                    // Check if current live block is the right tool
                    let is_correct_tool = match &renderer_guard.live_block {
                        Some(super::blocks::LiveBlockType::ToolUse(tool_block)) => {
                            tool_block.id == tool_id
                        }
                        _ => false,
                    };

                    if is_correct_tool {
                        renderer_guard.add_or_update_tool_parameter(&tool_id, name, value);
                    } else {
                        // This shouldn't happen if events are sent correctly,
                        // but we could start a new tool block here if needed
                        debug!(
                            "Received parameter update for tool {} but current block doesn't match",
                            tool_id
                        );
                    }
                }
            }
            UiEvent::EndTool { id: _ } => {
                // EndTool just marks the end of parameter streaming
                // The actual status comes later via UpdateToolStatus
                // For now, we don't change the status here - wait for UpdateToolStatus
            }
            UiEvent::AddImage {
                media_type,
                data: _,
            } => {
                debug!("Adding image: {}", media_type);

                // Add image placeholder to live block
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    renderer_guard.append_to_live_block(&format!("[image: {media_type}]\n"));
                }
            }
            UiEvent::StreamingStopped { id, cancelled } => {
                debug!("Streaming stopped (id: {}, cancelled: {})", id, cancelled);

                // Finalize the current live block
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let mut renderer_guard = renderer.lock().await;
                    let _ = renderer_guard.finalize_live_block();
                }
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
