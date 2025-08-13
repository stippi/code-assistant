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
    pub renderer: Arc<Mutex<Option<Arc<TerminalRenderer>>>>,
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

    pub async fn set_renderer_async(&self, renderer: Arc<TerminalRenderer>) {
        *self.renderer.lock().await = Some(renderer);
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
                state.update_pending_message(message);
            }
            UiEvent::UpdateToolStatus {
                tool_id,
                status,
                message: _,
                output: _,
            } => {
                debug!("Updating tool status for {}: {:?}", tool_id, status);
                state.tool_statuses.insert(tool_id, status);
            }
            UiEvent::ClearMessages => {
                debug!("Clearing messages");
                // No message state to clear in Terminal UI
            }
            UiEvent::DisplayUserInput {
                content,
                attachments,
            } => {
                debug!("Displaying user input: {}", content);

                // Print user input directly to the scrollable region via renderer
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let _ = renderer.append_content_chunk(&format!("\n> {content}\n"));
                    for attachment in &attachments {
                        match attachment {
                            crate::persistence::DraftAttachment::Text { content } => {
                                let _ = renderer.append_content_chunk(&format!(
                                    "  [attachment: text]\n{content}\n"
                                ));
                            }
                            crate::persistence::DraftAttachment::Image { mime_type, .. } => {
                                let _ = renderer.append_content_chunk(&format!(
                                    "  [attachment: image ({mime_type})]\n"
                                ));
                            }
                            crate::persistence::DraftAttachment::File { filename, .. } => {
                                let _ = renderer.append_content_chunk(&format!(
                                    "  [attachment: file ({filename}))]\n",
                                ));
                            }
                        }
                    }
                    let _ = renderer.append_content_chunk("\n");
                }
            }
            UiEvent::StreamingStarted(_request_id) => {
                debug!("Streaming started");
                // Ensure new stream starts at the beginning of a fresh line
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let _ = renderer.append_content_chunk("\n");
                }
            }
            UiEvent::AppendToTextBlock { content } => {
                debug!("Appending to text block: {}", content.trim());

                // Print to terminal using append_content_chunk for proper streaming
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let _ = renderer.append_content_chunk(&content);
                }
            }
            UiEvent::AppendToThinkingBlock { content } => {
                debug!("Appending to thinking block: {}", content.trim());

                // Print to terminal using append_content_chunk for proper streaming
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    if !content.trim().is_empty() {
                        let _ = renderer.append_content_chunk(&content);
                    }
                }
            }
            UiEvent::StartTool { name, id } => {
                debug!("Starting tool: {} ({})", name, id);

                // Print to terminal using write_message for tool headers
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let _ = renderer.append_content_chunk(&format!("\n• {name}\n"));
                }
            }
            UiEvent::UpdateToolParameter {
                tool_id: _,
                name,
                value,
            } => {
                debug!("Updating tool parameter: {} = {}", name, value.trim());

                // Print to terminal using write_message for tool parameters
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let _ =
                        renderer.append_content_chunk(&format!("  {}: {}\n", name, value.trim()));
                }
            }
            UiEvent::EndTool { id } => {
                debug!("Ending tool: {}", id);

                // Print to terminal for tool end
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let _ = renderer.append_content_chunk("\n");
                }
            }
            UiEvent::AddImage {
                media_type,
                data: _,
            } => {
                debug!("Adding image: {}", media_type);

                // Print to terminal for image placeholder
                if let Some(renderer) = self.renderer.lock().await.as_ref() {
                    let _ = renderer.append_content_chunk(&format!("[image: {media_type}]\n"));
                }
            }
            _ => {
                // For other events, just log them
                debug!("Unhandled event: {:?}", event);
            }
        }

        // Trigger redraw
        if let Some(tx) = self.redraw_tx.lock().await.as_ref() {
            let _ = tx.send(());
        }

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
        let rt = tokio::runtime::Handle::current();
        rt.spawn(async move {
            // Update state with rate limit info - we can't await here since this is not async
            // This will be handled via UiEvent in the backend
        });
    }

    fn clear_rate_limit(&self) {
        debug!("Rate limit cleared");
        let rt = tokio::runtime::Handle::current();
        rt.spawn(async move {
            // Clear rate limit info from state - we can't await here since this is not async
            // This will be handled via UiEvent in the backend
        });
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
