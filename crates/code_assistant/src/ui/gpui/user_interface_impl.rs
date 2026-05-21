//! `UserInterface` trait implementation for `Gpui`.
//!
//! This is the bridge between the agent system and the GUI — it receives
//! events and display fragments from the agent loop and forwards them into
//! the GPUI event queue for processing on the UI thread.

use crate::ui::{async_trait, DisplayFragment, UIError, UiEvent, UserInterface};

use super::*;

#[async_trait]
impl UserInterface for Gpui {
    async fn send_event(&self, event: UiEvent) -> Result<(), UIError> {
        // Handle special events that need state management
        match &event {
            UiEvent::StreamingStarted { request_id, .. } => {
                // Store the request ID
                *self.current_request_id.lock().unwrap() = *request_id;
                // Clear any existing error/notification when new operation starts
                *self.current_error.lock().unwrap() = None;
                *self.transient_status.lock().unwrap() = None;
            }
            UiEvent::StreamingStopped { .. } => {
                // Clear stop request for current session since streaming has stopped
                if let Some(current_session_id) = self.current_session_id.lock().unwrap().as_ref() {
                    self.session_stop_requests
                        .lock()
                        .unwrap()
                        .remove(current_session_id);
                }
            }
            UiEvent::UpdateSandboxPolicy { policy } => {
                *self.current_sandbox_policy.lock().unwrap() = Some(policy.clone());
            }
            _ => {}
        }

        // Forward all events to the event processing
        self.push_event(event);
        Ok(())
    }

    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError> {
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
                    error!("StreamingProcessor provided empty tool ID for parameter '{}' - this is a bug!", name);
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
                self.push_event(UiEvent::StartReasoningSummaryItem);
            }
            DisplayFragment::ReasoningSummaryDelta(delta) => {
                self.push_event(UiEvent::AppendReasoningSummaryDelta {
                    delta: delta.clone(),
                });
            }
            DisplayFragment::ReasoningComplete => {
                self.push_event(UiEvent::CompleteReasoning);
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

                self.push_event(UiEvent::AppendToolOutput {
                    tool_id: tool_id.clone(),
                    chunk: chunk.clone(),
                });
            }

            DisplayFragment::ToolTerminal { .. } => {
                // The GPUI terminal executor registers the tool→terminal
                // mapping directly in the TerminalPool, so no event needed.
            }

            DisplayFragment::CompactionDivider { summary } => {
                self.push_event(UiEvent::DisplayCompactionSummary {
                    summary: summary.clone(),
                });
            }
            DisplayFragment::HiddenToolCompleted => {
                self.push_event(UiEvent::HiddenToolCompleted);
            }
        }

        Ok(())
    }

    fn should_streaming_continue(&self) -> bool {
        // Check if the current session has requested a stop
        if let Some(current_session_id) = self.current_session_id.lock().unwrap().as_ref() {
            let stop_requests = self.session_stop_requests.lock().unwrap();
            if stop_requests.contains(current_session_id) {
                return false;
            }
        }

        // Default: continue streaming
        true
    }

    fn notify_rate_limit(&self, _seconds_remaining: u64) {
        // This is not handled here, but in the ProxyUI of each SessionInstance.
        // We receive separate events for SessionActivityState
    }

    fn clear_rate_limit(&self) {
        // See notify_rate_limit()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
