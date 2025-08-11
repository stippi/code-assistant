use crate::ui::{async_trait, DisplayFragment, UIError, UiEvent, UserInterface};
use std::any::Any;
use std::sync::Arc;
use tokio::sync::{watch, Mutex};
use tracing::debug;

use super::state::AppState;

pub struct TerminalTuiUI {
    app_state: Arc<Mutex<AppState>>,
    redraw_tx: Arc<Mutex<Option<watch::Sender<()>>>>,
    pub cancel_flag: Arc<Mutex<bool>>,
}

impl TerminalTuiUI {
    pub fn new() -> Self {
        Self {
            app_state: Arc::new(Mutex::new(AppState::new())),
            redraw_tx: Arc::new(Mutex::new(None)),
            cancel_flag: Arc::new(Mutex::new(false)),
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
}

#[async_trait]
impl UserInterface for TerminalTuiUI {
    async fn send_event(&self, event: UiEvent) -> Result<(), UIError> {
        let mut state = self.app_state.lock().await;

        match event {
            UiEvent::SetMessages {
                messages,
                session_id,
                tool_results,
            } => {
                debug!(
                    "Setting {} messages for session {:?}",
                    messages.len(),
                    session_id
                );
                state.clear_messages();
                for message in messages {
                    debug!(
                        "Adding message with role {:?} and {} fragments",
                        message.role,
                        message.fragments.len()
                    );
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
                state.clear_messages();
            }
            UiEvent::DisplayUserInput {
                content,
                attachments,
            } => {
                debug!("Displaying user input: {}", content);

                // Print user input directly to stdout so it appears above the inline viewport
                use std::io::{self, Write};
                println!("\n> {content}");

                // Print attachments if any
                for attachment in &attachments {
                    match attachment {
                        crate::persistence::DraftAttachment::Text { content } => {
                            println!("  [Attachment: Text]\n{content}");
                        }
                        crate::persistence::DraftAttachment::Image { mime_type, .. } => {
                            println!("  [Attachment: Image ({mime_type})]");
                        }
                        crate::persistence::DraftAttachment::File { filename, .. } => {
                            println!("  [Attachment: File ({filename})]");
                        }
                    }
                }
                println!(); // Empty line after user input
                io::stdout().flush().unwrap_or(());

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
                                crate::persistence::DraftAttachment::Image {
                                    mime_type,
                                    content,
                                } => {
                                    fragments.push(crate::ui::DisplayFragment::Image {
                                        media_type: mime_type,
                                        data: content,
                                    });
                                }
                                crate::persistence::DraftAttachment::File {
                                    content,
                                    filename,
                                    ..
                                } => {
                                    fragments.push(crate::ui::DisplayFragment::PlainText(format!(
                                        "File: {filename}\n{content}"
                                    )));
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
                if state.messages.is_empty()
                    || matches!(state.messages.last(), Some(msg) if msg.role == crate::ui::gpui::elements::MessageRole::User)
                {
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
                            if let crate::ui::DisplayFragment::PlainText(ref mut text) =
                                last_fragment
                            {
                                text.push_str(&content);
                            } else {
                                last_message
                                    .fragments
                                    .push(crate::ui::DisplayFragment::PlainText(content));
                            }
                        } else {
                            last_message
                                .fragments
                                .push(crate::ui::DisplayFragment::PlainText(content));
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
                            if let crate::ui::DisplayFragment::ThinkingText(ref mut text) =
                                last_fragment
                            {
                                text.push_str(&content);
                            } else {
                                last_message
                                    .fragments
                                    .push(crate::ui::DisplayFragment::ThinkingText(content));
                            }
                        } else {
                            last_message
                                .fragments
                                .push(crate::ui::DisplayFragment::ThinkingText(content));
                        }
                    }
                }
            }
            UiEvent::StartTool { name, id } => {
                debug!("Starting tool: {} ({})", name, id);
                // Add tool start to the last assistant message
                if let Some(last_message) = state.messages.last_mut() {
                    if last_message.role == crate::ui::gpui::elements::MessageRole::Assistant {
                        last_message
                            .fragments
                            .push(crate::ui::DisplayFragment::ToolName { name, id });
                    }
                }
            }
            UiEvent::UpdateToolParameter {
                tool_id,
                name,
                value,
            } => {
                debug!("Updating tool parameter: {} = {}", name, value.trim());
                // Add/update tool parameter in the last assistant message
                if let Some(last_message) = state.messages.last_mut() {
                    if last_message.role == crate::ui::gpui::elements::MessageRole::Assistant {
                        last_message
                            .fragments
                            .push(crate::ui::DisplayFragment::ToolParameter {
                                name,
                                value,
                                tool_id,
                            });
                    }
                }
            }
            UiEvent::EndTool { id } => {
                debug!("Ending tool: {}", id);
                // Add tool end to the last assistant message
                if let Some(last_message) = state.messages.last_mut() {
                    if last_message.role == crate::ui::gpui::elements::MessageRole::Assistant {
                        last_message
                            .fragments
                            .push(crate::ui::DisplayFragment::ToolEnd { id });
                    }
                }
            }
            UiEvent::AddImage { media_type, data } => {
                debug!("Adding image: {}", media_type);
                // Add image to the last message
                if let Some(last_message) = state.messages.last_mut() {
                    last_message
                        .fragments
                        .push(crate::ui::DisplayFragment::Image { media_type, data });
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
        // Print fragments directly to stdout for natural terminal scrolling
        use std::io::{self, Write};

        match fragment {
            DisplayFragment::PlainText(text) => {
                debug!("Fragment: PlainText({})", text.trim());
                print!("{}", text);
                io::stdout().flush().unwrap_or(());
            }
            DisplayFragment::ThinkingText(text) => {
                debug!("Fragment: ThinkingText({})", text.trim());
                // For thinking text, we could style it differently or skip it
                // For now, let's show it in a subtle way
                if !text.trim().is_empty() {
                    print!("{}", text);
                    io::stdout().flush().unwrap_or(());
                }
            }
            DisplayFragment::ToolName { name, id } => {
                debug!("Fragment: ToolName({}, {})", name, id);
                println!("\n[Tool: {}]", name);
                io::stdout().flush().unwrap_or(());
            }
            DisplayFragment::ToolParameter {
                name,
                value,
                tool_id,
            } => {
                debug!(
                    "Fragment: ToolParameter({}, {}, {})",
                    name,
                    value.trim(),
                    tool_id
                );
                println!("  {}: {}", name, value.trim());
                io::stdout().flush().unwrap_or(());
            }
            DisplayFragment::ToolEnd { id } => {
                debug!("Fragment: ToolEnd({})", id);
                println!("  [Tool completed]\n");
                io::stdout().flush().unwrap_or(());
            }
            DisplayFragment::Image {
                media_type,
                data: _,
            } => {
                debug!("Fragment: Image({})", media_type);
                println!("[Image: {}]", media_type);
                io::stdout().flush().unwrap_or(());
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
