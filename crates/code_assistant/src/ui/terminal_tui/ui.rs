use crate::ui::terminal_tui::state::AppState;
use crate::ui::{async_trait, DisplayFragment, UIError, UiEvent, UserInterface};
use std::any::Any;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tracing::debug;

pub struct TerminalTuiUI {
    app_state: Arc<Mutex<AppState>>,
    cancel_flag: Arc<Mutex<bool>>,
    redraw_tx: Arc<Mutex<Option<mpsc::UnboundedSender<()>>>>,
}

impl TerminalTuiUI {
    pub fn new(app_state: Arc<Mutex<AppState>>) -> Self {
        Self {
            app_state,
            cancel_flag: Arc::new(Mutex::new(false)),
            redraw_tx: Arc::new(Mutex::new(None)),
        }
    }

    #[allow(dead_code)]
    pub async fn set_cancel_flag(&self, cancelled: bool) {
        *self.cancel_flag.lock().await = cancelled;
    }

    pub async fn set_redraw_channel(&self, tx: mpsc::UnboundedSender<()>) {
        *self.redraw_tx.lock().await = Some(tx);
    }

    async fn trigger_redraw(&self) {
        if let Some(tx) = self.redraw_tx.lock().await.as_ref() {
            let _ = tx.send(());
        }
    }


}

#[async_trait]
impl UserInterface for TerminalTuiUI {
    async fn send_event(&self, event: UiEvent) -> Result<(), UIError> {
        debug!("TerminalTuiUI received event: {:?}", event);

        let mut state = self.app_state.lock().await;

        match event {
            UiEvent::SetMessages { messages, session_id, tool_results } => {
                debug!("Setting {} messages for session {:?}", messages.len(), session_id);
                    debug!("Adding message with role {:?} and {} fragments", message.role, message.fragments.len());
                    for (i, fragment) in message.fragments.iter().enumerate() {
                        match fragment {
                            crate::ui::DisplayFragment::PlainText(text) => {
                                debug!("  Fragment {}: PlainText({} chars)", i, text.len());
                            }
                            crate::ui::DisplayFragment::ThinkingText(text) => {
                                debug!("  Fragment {}: ThinkingText({} chars)", i, text.len());
                            }
                            _ => {
                                debug!("  Fragment {}: {:?}", i, fragment);
                            }
                        }
                    }
                    state.add_message(message);
                }
                if let Some(session_id) = session_id {
                    state.set_current_session(Some(session_id));
                }

                // Update tool statuses from tool results
                for tool_result in tool_results {
                    state.tool_statuses.insert(tool_result.tool_id, tool_result.status);
                }
            }
            UiEvent::UpdateMemory { memory } => {
                debug!("Updating working memory");
                state.update_working_memory(memory);
            }
            UiEvent::UpdateChatList { sessions } => {
                debug!("Updating chat list with {} sessions", sessions.len());
                state.update_sessions(sessions);
            }
            UiEvent::UpdateSessionActivityState { session_id, activity_state } => {
                debug!("Updating activity state for session {}: {:?}", session_id, activity_state);
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
            UiEvent::UpdateToolStatus { tool_id, status, message: _, output: _ } => {
                debug!("Updating tool status for {}: {:?}", tool_id, status);
                state.tool_statuses.insert(tool_id, status);
            }
            UiEvent::ClearMessages => {
                debug!("Clearing messages");
            UiEvent::DisplayUserInput { content, attachments } => {
                debug!("Displaying user input: {}", content);
                // Add user message to state
                let user_message = crate::ui::ui_events::MessageData {
                    role: crate::ui::gpui::elements::MessageRole::User,
                    fragments: {
                        let mut fragments = vec![crate::ui::DisplayFragment::PlainText(content)];
                        // Add attachment fragments
                        for attachment in attachments {
                            match attachment {
                                crate::persistence::DraftAttachment::Text { content } => {
                                    fragments.push(crate::ui::DisplayFragment::PlainText(content));
                                }
                                crate::persistence::DraftAttachment::Image { mime_type, content } => {
                                    fragments.push(crate::ui::DisplayFragment::Image {
                                        media_type: mime_type,
                                        data: content
                                    });
                                }
                                crate::persistence::DraftAttachment::File { content, filename, .. } => {
                                    fragments.push(crate::ui::DisplayFragment::PlainText(
                                        format!("File: {filename}\n{content}")
                                    ));
                                }
                            }
                        }
                        fragments
                    },
                };
                state.add_message(user_message);
            }
            UiEvent::StreamingStarted(_request_id) => {
                debug!("Streaming started");
                // Ensure we have an assistant message to append to
                if state.messages.is_empty() ||
                   matches!(state.messages.last(), Some(msg) if msg.role == crate::ui::gpui::elements::MessageRole::User) {
                    let assistant_message = crate::ui::ui_events::MessageData {
                        role: crate::ui::gpui::elements::MessageRole::Assistant,
                        fragments: Vec::new(),
                    };
                    state.add_message(assistant_message);
                }
            }
            UiEvent::AppendToTextBlock { content } => {
                debug!("Appending to text block: {}", content.trim());
                // Append to the last assistant message
                if let Some(last_message) = state.messages.last_mut() {
                    if last_message.role == crate::ui::gpui::elements::MessageRole::Assistant {
                        // Try to append to existing PlainText fragment or create new one
                        if let Some(last_fragment) = last_message.fragments.last_mut() {
                            if let crate::ui::DisplayFragment::PlainText(ref mut text) = last_fragment {
                                text.push_str(&content);
                            } else {
                                last_message.fragments.push(crate::ui::DisplayFragment::PlainText(content));
                            }
                        } else {
                            last_message.fragments.push(crate::ui::DisplayFragment::PlainText(content));
                        }

                    }
                }
            }
            UiEvent::AppendToThinkingBlock { content } => {
                debug!("Appending to thinking block: {}", content.trim());
                // Append to the last assistant message
                if let Some(last_message) = state.messages.last_mut() {
                    if last_message.role == crate::ui::gpui::elements::MessageRole::Assistant {
                        // Try to append to existing ThinkingText fragment or create new one
                        if let Some(last_fragment) = last_message.fragments.last_mut() {
                            if let crate::ui::DisplayFragment::ThinkingText(ref mut text) = last_fragment {
                                text.push_str(&content);
                            } else {
                                last_message.fragments.push(crate::ui::DisplayFragment::ThinkingText(content));
                            }
                        } else {
                            last_message.fragments.push(crate::ui::DisplayFragment::ThinkingText(content));
                        }
                    }
                }
            }
            UiEvent::StartTool { name, id } => {
                debug!("Starting tool: {} ({})", name, id);
                // Add tool start to the last assistant message
                if let Some(last_message) = state.messages.last_mut() {
                    if last_message.role == crate::ui::gpui::elements::MessageRole::Assistant {
                        last_message.fragments.push(crate::ui::DisplayFragment::ToolName { name, id });
                    }
                }
            }
            UiEvent::UpdateToolParameter { tool_id, name, value } => {
                debug!("Updating tool parameter: {} = {}", name, value.trim());
                // Add/update tool parameter in the last assistant message
                if let Some(last_message) = state.messages.last_mut() {
                    if last_message.role == crate::ui::gpui::elements::MessageRole::Assistant {
                        last_message.fragments.push(crate::ui::DisplayFragment::ToolParameter {
                            name,
                            value,
                            tool_id
                        });
                    }
                }
            }
            UiEvent::EndTool { id } => {
                debug!("Ending tool: {}", id);
                // Add tool end to the last assistant message
                if let Some(last_message) = state.messages.last_mut() {
                    if last_message.role == crate::ui::gpui::elements::MessageRole::Assistant {
                        last_message.fragments.push(crate::ui::DisplayFragment::ToolEnd { id });
                    }
                }
            }
            UiEvent::AddImage { media_type, data } => {
                debug!("Adding image: {}", media_type);
                // Add image to the last message
                if let Some(last_message) = state.messages.last_mut() {
                    last_message.fragments.push(crate::ui::DisplayFragment::Image { media_type, data });
                }
            }
            _ => {
                // For other events, just log them
                debug!("Unhandled event: {:?}", event);
            }
        }

        // Trigger redraw
        drop(state); // Release the lock before async call
        self.trigger_redraw().await;
        Ok(())
    }
        // Handle streaming fragments directly by updating the state
        let app_state = self.app_state.clone();
        let redraw_tx = self.redraw_tx.clone();
        let fragment = fragment.clone();

        let rt = tokio::runtime::Handle::current();
        rt.spawn(async move {
            let mut state = app_state.lock().await;

            match &fragment {
                DisplayFragment::PlainText(text) => {
                    debug!("Fragment: PlainText({})", text.trim());
                    // Append to last assistant message
                    if let Some(last_message) = state.messages.last_mut() {
                        if last_message.role == crate::ui::gpui::elements::MessageRole::Assistant {
                            if let Some(last_fragment) = last_message.fragments.last_mut() {
                                if let crate::ui::DisplayFragment::PlainText(ref mut existing_text) = last_fragment {
                                    existing_text.push_str(text);
                                } else {
                                    last_message.fragments.push(crate::ui::DisplayFragment::PlainText(text.clone()));
                                }
                            } else {
                                last_message.fragments.push(crate::ui::DisplayFragment::PlainText(text.clone()));
                            }
                        }
                    }
                }
                DisplayFragment::ThinkingText(text) => {
                    debug!("Fragment: ThinkingText({})", text.trim());
                    // Append to last assistant message
                    if let Some(last_message) = state.messages.last_mut() {
                        if last_message.role == crate::ui::gpui::elements::MessageRole::Assistant {
                            if let Some(last_fragment) = last_message.fragments.last_mut() {
                                if let crate::ui::DisplayFragment::ThinkingText(ref mut existing_text) = last_fragment {
                                    existing_text.push_str(text);
                                } else {
                                    last_message.fragments.push(crate::ui::DisplayFragment::ThinkingText(text.clone()));
                                }
                            } else {
                                last_message.fragments.push(crate::ui::DisplayFragment::ThinkingText(text.clone()));
                            }
                        }
                    }
                }
                DisplayFragment::ToolName { name, id } => {
                    debug!("Fragment: ToolName({}, {})", name, id);
                    if let Some(last_message) = state.messages.last_mut() {
                        if last_message.role == crate::ui::gpui::elements::MessageRole::Assistant {
                            last_message.fragments.push(fragment.clone());
                        }
                    }
                }
                DisplayFragment::ToolParameter { name, value, tool_id } => {
                    debug!("Fragment: ToolParameter({}, {}, {})", name, value.trim(), tool_id);
                    if let Some(last_message) = state.messages.last_mut() {
                        if last_message.role == crate::ui::gpui::elements::MessageRole::Assistant {
                            last_message.fragments.push(fragment.clone());
                        }
                    }
                }
                DisplayFragment::ToolEnd { id } => {
                    debug!("Fragment: ToolEnd({})", id);
                    if let Some(last_message) = state.messages.last_mut() {
                        if last_message.role == crate::ui::gpui::elements::MessageRole::Assistant {
                            last_message.fragments.push(fragment.clone());
                        }
                    }
                }
                DisplayFragment::Image { media_type, data: _ } => {
                    debug!("Fragment: Image({})", media_type);
                    if let Some(last_message) = state.messages.last_mut() {
                        last_message.fragments.push(fragment.clone());
                    }
                }
            }

            // Trigger redraw
            if let Some(tx) = redraw_tx.lock().await.as_ref() {
                let _ = tx.send(());
            }
        });

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
