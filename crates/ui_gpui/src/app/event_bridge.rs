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
            EventPayload::Ui(UiEvent::HandoffPrepared { prompt }) if !is_current_session => {
                // Offered in the background session's draft, unless the
                // user already left one there.
                if let Some(session_id) = event.session_id
                    && self.load_draft_for_session(&session_id).is_none()
                {
                    self.save_draft_for_session(&session_id, &handoff_draft(&prompt), &[], None);
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
                    // Prompts kept for the session that asked them may be
                    // settled while another one is viewed.
                    UiEvent::NewContextTargetResolved { .. }
                    | UiEvent::ToolPermissionRequestResolved { .. } => true,
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
        self.restore_prompts(snapshot);
        for event in snapshot.connect_events() {
            self.push_event(event);
        }
    }

    /// Take over the snapshot's open prompts. It is authoritative for its
    /// session, also after a lag that swallowed a resolution.
    fn restore_prompts(&self, snapshot: &SessionSnapshot) {
        let session_id = &snapshot.session_id;

        let mut permissions = self.pending_permission_requests.lock().unwrap();
        permissions.retain(|(asking, _)| asking != session_id);
        permissions.extend(
            snapshot
                .pending_permission_requests
                .iter()
                .map(|request| (session_id.clone(), request.clone())),
        );
        drop(permissions);

        let mut target = self.pending_new_context_target.lock().unwrap();
        if let Some(request) = &snapshot.pending_new_context_target {
            *target = Some((session_id.clone(), request.clone()));
        } else if target
            .as_ref()
            .is_some_and(|(asking, _)| asking == session_id)
        {
            *target = None;
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
                // Only the viewed session's events get here.
                if let Some(session_id) = self.get_current_session_id() {
                    let mut pending = self.pending_permission_requests.lock().unwrap();
                    if !pending
                        .iter()
                        .any(|(_, r)| r.request_id == request.request_id)
                    {
                        pending.push((session_id, request.clone()));
                    }
                }
            }
            UiEvent::ToolPermissionRequestResolved { request_id } => {
                self.pending_permission_requests
                    .lock()
                    .unwrap()
                    .retain(|(_, r)| &r.request_id != request_id);
            }
            UiEvent::RequestNewContextTarget { request } => {
                // Only the viewed session's events get here.
                if let Some(session_id) = self.get_current_session_id() {
                    *self.pending_new_context_target.lock().unwrap() =
                        Some((session_id, request.clone()));
                }
            }
            UiEvent::NewContextTargetResolved { request_id } => {
                let mut pending = self.pending_new_context_target.lock().unwrap();
                if pending
                    .as_ref()
                    .is_some_and(|(_, request)| &request.request_id == request_id)
                {
                    *pending = None;
                }
            }
            UiEvent::HandoffPrepared { prompt } => {
                if let Some(session_id) = self.get_current_session_id() {
                    *self.prepared_handoff.lock().unwrap() =
                        Some((session_id, handoff_draft(prompt)));
                }
            }
            UiEvent::SessionHandedOff { to } => {
                // Follow the work into the new session.
                *self.current_session_id.lock().unwrap() = Some(to.clone());
                self.cmd_refresh_chat_list();
                self.cmd_load_session(to.clone(), None);
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
            DisplayFragment::ThinkingText { text, .. } => {
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
            DisplayFragment::ToolTerminalExited { tool_id, exit_code } => {
                self.push_event(UiEvent::SetToolTerminalExited {
                    tool_id: tool_id.clone(),
                    exit_code: *exit_code,
                });
            }
            DisplayFragment::ToolTerminal { tool_id, .. } => {
                // A backend terminal exists for this tool (possibly before
                // any output). Create the card's display terminal now so
                // the card leaves its skeleton state and shows the running
                // terminal with its stop button — even for silent commands.
                self.push_event(UiEvent::AttachToolTerminal {
                    tool_id: tool_id.clone(),
                });
            }
            DisplayFragment::ContextDivider { boundary, summary } => {
                self.push_event(UiEvent::DisplayContextDivider {
                    boundary: *boundary,
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

/// The composer text offering a prepared handoff.
fn handoff_draft(prompt: &str) -> String {
    format!("/new {prompt}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_assistant_core::session::new_context::NewContextTargetRequest;
    use code_assistant_core::session::permissions::ToolPermissionRequestData;

    fn viewing(session_id: &str) -> Gpui {
        let gpui = Gpui::new();
        *gpui.current_session_id.lock().unwrap() = Some(session_id.to_string());
        gpui
    }

    fn ui_event(session_id: &str, event: UiEvent) -> SessionEvent {
        SessionEvent {
            session_id: Some(session_id.to_string()),
            payload: EventPayload::Ui(event),
        }
    }

    fn request(id: &str) -> NewContextTargetRequest {
        NewContextTargetRequest {
            request_id: id.to_string(),
            options: Vec::new(),
        }
    }

    fn permission_request(id: &str) -> ToolPermissionRequestData {
        ToolPermissionRequestData {
            request_id: id.to_string(),
            tool_id: None,
            tool_name: "delete_files".to_string(),
            summary: String::new(),
            metadata: serde_json::Value::Null,
            options: Vec::new(),
        }
    }

    fn pending_permission_ids(gpui: &Gpui, session_id: &str) -> Vec<String> {
        gpui.get_pending_permission_requests(session_id)
            .into_iter()
            .map(|request| request.request_id)
            .collect()
    }

    fn snapshot(
        session_id: &str,
        pending_permission_requests: Vec<ToolPermissionRequestData>,
    ) -> SessionSnapshot {
        let now = std::time::SystemTime::now();
        SessionSnapshot {
            session_id: session_id.to_string(),
            messages: Vec::new(),
            tool_results: Vec::new(),
            plan: Default::default(),
            activity_state: Default::default(),
            metadata: code_assistant_core::persistence::ChatMetadata {
                id: session_id.to_string(),
                name: String::new(),
                created_at: now,
                updated_at: now,
                message_count: 0,
                total_usage: Default::default(),
                last_usage: Default::default(),
                tokens_limit: None,
                tool_syntax: code_assistant_core::types::ToolSyntax::Native,
                initial_project: String::new(),
                plan_collapsed: false,
                is_resumable: false,
            },
            pending_message: None,
            current_model: String::new(),
            allowed_models: Vec::new(),
            sandbox_policy: Default::default(),
            permission_tier: Default::default(),
            mcp_servers: Vec::new(),
            pending_permission_requests,
            pending_new_context_target: None,
        }
    }

    /// The bridge runs on GPUI's executor, outside any tokio runtime.
    #[test]
    fn a_handoff_prepared_in_the_background_becomes_its_draft() {
        let gpui = viewing("a");

        futures::executor::block_on(gpui.handle_stream_event(ui_event(
            "b",
            UiEvent::HandoffPrepared {
                prompt: "Next step".into(),
            },
        )));

        let (draft, _, _) = gpui.load_draft_for_session("b").expect("a draft");
        assert_eq!(draft, "/new Next step");
    }

    #[test]
    fn a_target_question_shows_only_in_the_asking_session() {
        let gpui = viewing("a");
        futures::executor::block_on(gpui.handle_stream_event(ui_event(
            "a",
            UiEvent::RequestNewContextTarget {
                request: request("q1"),
            },
        )));
        assert!(gpui.get_pending_new_context_target("a").is_some());

        // Viewing another session: not shown there, but still settled.
        *gpui.current_session_id.lock().unwrap() = Some("b".to_string());
        assert!(gpui.get_pending_new_context_target("b").is_none());
        futures::executor::block_on(gpui.handle_stream_event(ui_event(
            "a",
            UiEvent::NewContextTargetResolved {
                request_id: "q1".into(),
            },
        )));
        assert!(gpui.get_pending_new_context_target("a").is_none());
    }

    #[test]
    fn a_permission_prompt_shows_only_in_the_asking_session() {
        let gpui = viewing("a");
        futures::executor::block_on(gpui.handle_stream_event(ui_event(
            "a",
            UiEvent::RequestToolPermission {
                request: permission_request("p1"),
            },
        )));
        assert_eq!(pending_permission_ids(&gpui, "a"), ["p1"]);

        // Switching away leaves it with the session that asked.
        *gpui.current_session_id.lock().unwrap() = Some("b".to_string());
        assert!(pending_permission_ids(&gpui, "b").is_empty());
        assert_eq!(pending_permission_ids(&gpui, "a"), ["p1"]);
    }

    #[test]
    fn a_permission_prompt_settled_in_the_background_goes_away() {
        let gpui = viewing("a");
        for id in ["p1", "p2"] {
            futures::executor::block_on(gpui.handle_stream_event(ui_event(
                "a",
                UiEvent::RequestToolPermission {
                    request: permission_request(id),
                },
            )));
        }

        *gpui.current_session_id.lock().unwrap() = Some("b".to_string());
        futures::executor::block_on(gpui.handle_stream_event(ui_event(
            "a",
            UiEvent::ToolPermissionRequestResolved {
                request_id: "p1".into(),
            },
        )));
        assert_eq!(pending_permission_ids(&gpui, "a"), ["p2"]);
    }

    #[test]
    fn a_snapshot_restores_the_permission_prompts_of_its_session() {
        let gpui = viewing("a");
        for (session_id, id) in [("a", "stale"), ("b", "other")] {
            *gpui.current_session_id.lock().unwrap() = Some(session_id.to_string());
            futures::executor::block_on(gpui.handle_stream_event(ui_event(
                session_id,
                UiEvent::RequestToolPermission {
                    request: permission_request(id),
                },
            )));
        }

        // Authoritative for its session, e.g. after a lag swallowed the
        // resolution of "stale"; other sessions keep theirs.
        gpui.apply_snapshot(&snapshot(
            "a",
            vec![permission_request("p1"), permission_request("p2")],
        ));
        assert_eq!(pending_permission_ids(&gpui, "a"), ["p1", "p2"]);
        assert_eq!(pending_permission_ids(&gpui, "b"), ["other"]);
    }

    #[test]
    fn a_snapshot_restores_the_target_question_of_its_session() {
        // Asked while another session was viewed, so the bridge dropped it.
        let gpui = viewing("b");
        let mut connected = snapshot("a", Vec::new());
        connected.pending_new_context_target = Some(request("q1"));

        gpui.apply_snapshot(&connected);
        assert!(gpui.get_pending_new_context_target("a").is_some());

        gpui.apply_snapshot(&snapshot("a", Vec::new()));
        assert!(gpui.get_pending_new_context_target("a").is_none());
    }
}
