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
                state.clear_messages();
                for message in messages {
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
                state.clear_messages();
            }
            _ => {
                // For other events, we'll handle them in Phase 4 when we implement rendering
                debug!("Ignoring event for now: {:?}", event);
            }
        }

        // Trigger redraw
        drop(state); // Release the lock before async call
        self.trigger_redraw().await;
        Ok(())
    }

    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError> {
        // For now, just log the fragment - we'll implement actual display in Phase 4
        match fragment {
            DisplayFragment::PlainText(text) => {
                debug!("Fragment: PlainText({})", text.trim());
            }
            DisplayFragment::ThinkingText(text) => {
                debug!("Fragment: ThinkingText({})", text.trim());
            }
            DisplayFragment::ToolName { name, id } => {
                debug!("Fragment: ToolName({}, {})", name, id);
            }
            DisplayFragment::ToolParameter { name, value, tool_id } => {
                debug!("Fragment: ToolParameter({}, {}, {})", name, value.trim(), tool_id);
            }
            DisplayFragment::ToolEnd { id } => {
                debug!("Fragment: ToolEnd({})", id);
            }
            DisplayFragment::Image { media_type, data: _ } => {
                debug!("Fragment: Image({})", media_type);
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
