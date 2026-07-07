//! Subscription to the core→UI broadcast stream.
//!
//! The bridge is GPUI's single ingestion point for everything the core
//! publishes. It filters by the currently viewed session — sidebar-relevant
//! events (activity, metadata, chat list) pass regardless — and forwards
//! into the internal UI event queue, where the existing processing on the
//! foreground thread takes over. On lag it resyncs by reloading a fresh
//! snapshot of the viewed session.

use code_assistant_core::session::{EventPayload, SessionEvent, SessionSnapshot, StreamError};
use code_assistant_core::ui::UiEvent;
use tracing::{debug, warn};

use super::super::*;

impl Gpui {
    /// Subscribe to the broadcast stream and forward events until it closes.
    /// Called once from `run_app`.
    pub(crate) fn spawn_event_bridge(&self) {
        let Some(service) = self.session_service() else {
            warn!("No session service — event bridge not started");
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            let mut subscription = service.subscribe();
            debug!("Event bridge started");
            loop {
                match subscription.recv().await {
                    Ok(event) => gpui.handle_stream_event(event).await,
                    Err(StreamError::Lagged { missed }) => {
                        warn!("Event stream lagged ({missed} events missed) — resyncing");
                        if let Some(session_id) = gpui.get_current_session_id() {
                            gpui.cmd_load_session(session_id, None);
                        }
                    }
                    Err(StreamError::Closed) => {
                        debug!("Event stream closed — bridge stopped");
                        break;
                    }
                }
            }
        });
    }

    /// Apply one stream event: decide whether it concerns this view, then
    /// feed it into the internal UI event queue.
    async fn handle_stream_event(&self, event: SessionEvent) {
        let current = self.get_current_session_id();
        let is_current_session = event.session_id == current;

        match event.payload {
            EventPayload::Fragment(fragment) => {
                // Streaming fragments only matter for the viewed session;
                // background sessions are resynced via snapshot on switch.
                if is_current_session {
                    let _ = self.handle_fragment(&fragment);
                }
            }
            EventPayload::Ui(ui_event) => {
                let forward = match &ui_event {
                    // Sidebar state: relevant for every session, always.
                    UiEvent::UpdateSessionActivityState { .. }
                    | UiEvent::UpdateSessionMetadata { .. }
                    | UiEvent::UpdateChatList { .. }
                    | UiEvent::RefreshChatList
                    | UiEvent::ConfigChanged => true,
                    // Everything else: app-scoped events pass, session-scoped
                    // events only for the viewed session.
                    _ => event.session_id.is_none() || is_current_session,
                };
                if forward {
                    let _ = self.handle_app_event(ui_event).await;
                }
            }
        }
    }

    /// Apply an owned session snapshot by replaying the canonical connect
    /// sequence through the internal event queue.
    pub fn apply_snapshot(&self, snapshot: &SessionSnapshot) {
        for event in snapshot.connect_events() {
            self.push_event(event);
        }
    }

    /// Ingest an application event: track side state, then enqueue it for
    /// processing on the foreground thread.
    pub(crate) async fn handle_app_event(&self, event: UiEvent) {
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
            UiEvent::UpdatePermissionTier { tier } => {
                *self.current_permission_tier.lock().unwrap() = Some(*tier);
            }
            UiEvent::RequestToolPermission { request } => {
                let mut pending = self.pending_permission_requests.lock().unwrap();
                if !pending.iter().any(|r| r.request_id == request.request_id) {
                    pending.push(request.clone());
                }
            }
            UiEvent::ToolPermissionRequestResolved { request_id } => {
                self.pending_permission_requests
                    .lock()
                    .unwrap()
                    .retain(|r| &r.request_id != request_id);
            }
            _ => {}
        }

        // Forward all events to the event processing
        self.push_event(event);
    }

    /// Translate a streaming display fragment of the viewed session into
    /// the internal event vocabulary.
    pub(crate) fn handle_fragment(
        &self,
        fragment: &code_assistant_core::ui::DisplayFragment,
    ) -> Result<(), code_assistant_core::ui::UIError> {
        use code_assistant_core::ui::{DisplayFragment, UIError};

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
                    tracing::error!(
                        "StreamingProcessor provided empty tool ID for parameter '{}' - this is a bug!",
                        name
                    );
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
            DisplayFragment::ToolTerminalOutput { tool_id, bytes } => {
                self.push_event(UiEvent::AppendToolTerminalOutput {
                    tool_id: tool_id.clone(),
                    bytes: bytes.clone(),
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
}
