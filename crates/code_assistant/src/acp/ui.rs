use agent_client_protocol as acp;
use async_trait::async_trait;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::acp::types::{fragment_to_content_block, map_tool_kind, map_tool_status};
use crate::ui::{DisplayFragment, UIError, UiEvent, UserInterface};

/// UserInterface implementation that sends session/update notifications via ACP
pub struct ACPUserUI {
    session_id: acp::SessionId,
    session_update_tx: mpsc::UnboundedSender<(acp::SessionNotification, oneshot::Sender<()>)>,
    // Track tool calls for status updates
    tool_calls: Arc<Mutex<HashMap<String, ToolCallState>>>,
    // Track if we should continue streaming (atomic for lock-free access from sync callbacks)
    should_continue: Arc<AtomicBool>,
}

#[derive(Default, Clone)]
struct ParameterValue {
    value: String,
}

impl ParameterValue {
    fn append(&mut self, chunk: &str) {
        self.value.push_str(chunk);
    }
}

struct ToolCallState {
    id: acp::ToolCallId,
    tool_name: Option<String>,
    title: Option<String>,
    kind: Option<acp::ToolKind>,
    status: acp::ToolCallStatus,
    parameters: BTreeMap<String, ParameterValue>,
    output_stream: Option<String>,
    final_output: Option<String>,
    status_message: Option<String>,
}

impl ToolCallState {
    fn new(id: &str) -> Self {
        Self {
            id: acp::ToolCallId(id.to_string().into()),
            tool_name: None,
            title: None,
            kind: None,
            status: acp::ToolCallStatus::Pending,
            parameters: BTreeMap::new(),
            output_stream: None,
            final_output: None,
            status_message: None,
        }
    }

    fn set_tool_name(&mut self, name: &str) {
        self.tool_name = Some(name.to_string());
        self.title.get_or_insert_with(|| name.to_string());
        self.kind.get_or_insert_with(|| map_tool_kind(name));
    }

    fn kind(&self) -> acp::ToolKind {
        self.kind.clone().unwrap_or(acp::ToolKind::Other)
    }

    fn status(&self) -> acp::ToolCallStatus {
        self.status
    }

    fn append_parameter(&mut self, name: &str, value: &str) {
        let entry = self
            .parameters
            .entry(name.to_string())
            .or_insert_with(ParameterValue::default);
        entry.append(value);
    }

    fn update_status(
        &mut self,
        status: acp::ToolCallStatus,
        message: Option<String>,
        output: Option<String>,
    ) {
        self.status = status;
        if let Some(message) = message {
            if !message.is_empty() {
                self.status_message = Some(message);
            }
        }
        if let Some(output) = output {
            self.final_output = Some(output);
        }
    }

    fn ensure_completed(&mut self) {
        if matches!(
            self.status,
            acp::ToolCallStatus::Pending | acp::ToolCallStatus::InProgress
        ) {
            self.status = acp::ToolCallStatus::Completed;
        }
    }

    fn append_output_chunk(&mut self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        self.output_stream
            .get_or_insert_with(String::new)
            .push_str(chunk);
    }

    fn raw_input(&self) -> Option<JsonValue> {
        if self.parameters.is_empty() {
            return None;
        }

        let mut map = JsonMap::new();
        for (key, value) in &self.parameters {
            map.insert(key.clone(), parse_parameter_value(&value.value));
        }
        Some(JsonValue::Object(map))
    }

    fn output_text(&self) -> Option<String> {
        if let Some(final_output) = &self.final_output {
            Some(final_output.clone())
        } else {
            self.output_stream.clone()
        }
    }

    fn raw_output(&self) -> Option<JsonValue> {
        self.output_text().map(|text| JsonValue::String(text))
    }

    fn diff_content(&self) -> Option<acp::ToolCallContent> {
        if !matches!(self.tool_name.as_deref(), Some("edit")) {
            return None;
        }

        let path = self.parameters.get("path")?.value.trim();
        if path.is_empty() {
            return None;
        }
        let new_text = self.parameters.get("new_text")?.value.clone();
        let old_text = self.parameters.get("old_text").map(|v| v.value.clone());

        let diff = acp::Diff {
            path: PathBuf::from(path),
            old_text,
            new_text,
        };

        Some(acp::ToolCallContent::Diff { diff })
    }

    fn build_content(&self) -> Option<Vec<acp::ToolCallContent>> {
        let mut content = Vec::new();

        if let Some(diff_content) = self.diff_content() {
            content.push(diff_content);

            let supplemental = self
                .parameters
                .iter()
                .filter(|(name, _)| *name != "old_text" && *name != "new_text")
                .map(|(name, value)| format!("{name}: {}", value.value))
                .collect::<Vec<_>>();

            if !supplemental.is_empty() {
                content.push(text_content(supplemental.join("\n")));
            }
        } else if !self.parameters.is_empty() {
            let mut lines = Vec::new();
            for (name, value) in &self.parameters {
                lines.push(format!("{name}: {}", value.value));
            }
            content.push(text_content(lines.join("\n")));
        }

        if let Some(message) = &self.status_message {
            if !message.is_empty() {
                content.push(text_content(message.clone()));
            }
        }

        if let Some(output) = self.output_text() {
            if !output.is_empty() {
                content.push(text_content(output));
            }
        }

        if content.is_empty() {
            None
        } else {
            Some(content)
        }
    }

    fn to_tool_call(&self) -> acp::ToolCall {
        acp::ToolCall {
            id: self.id.clone(),
            title: self
                .title
                .clone()
                .or_else(|| self.tool_name.clone())
                .unwrap_or_default(),
            kind: self.kind(),
            status: self.status(),
            content: self.build_content().unwrap_or_default(),
            locations: Vec::new(),
            raw_input: self.raw_input(),
            raw_output: self.raw_output(),
        }
    }

    fn to_update(&self) -> acp::ToolCallUpdate {
        acp::ToolCallUpdate {
            id: self.id.clone(),
            fields: acp::ToolCallUpdateFields {
                kind: self.kind.clone(),
                status: Some(self.status()),
                title: self.title.clone(),
                content: self.build_content(),
                locations: None,
                raw_input: self.raw_input(),
                raw_output: self.raw_output(),
            },
        }
    }
}

fn parse_parameter_value(raw: &str) -> JsonValue {
    if raw.is_empty() {
        return JsonValue::String(String::new());
    }

    let trimmed = raw.trim();
    if let Ok(value) = serde_json::from_str::<JsonValue>(trimmed) {
        return value;
    }

    JsonValue::String(raw.to_string())
}

fn text_content(text: String) -> acp::ToolCallContent {
    acp::ToolCallContent::Content {
        content: acp::ContentBlock::Text(acp::TextContent {
            annotations: None,
            text,
        }),
    }
}

impl ACPUserUI {
    pub fn new(
        session_id: acp::SessionId,
        session_update_tx: mpsc::UnboundedSender<(acp::SessionNotification, oneshot::Sender<()>)>,
    ) -> Self {
        Self {
            session_id,
            session_update_tx,
            tool_calls: Arc::new(Mutex::new(HashMap::new())),
            should_continue: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Signal that the operation should be cancelled
    /// This is called by the cancel() method to stop the prompt() loop
    pub fn signal_cancel(&self) {
        self.should_continue.store(false, Ordering::Relaxed);
    }

    /// Send a session update notification
    async fn send_session_update(&self, update: acp::SessionUpdate) -> Result<(), UIError> {
        tracing::debug!("ACPUserUI: Sending session update: {:?}", update);
        let (tx, rx) = oneshot::channel();
        self.session_update_tx
            .send((
                acp::SessionNotification {
                    session_id: self.session_id.clone(),
                    update,
                },
                tx,
            ))
            .map_err(|_| {
                tracing::error!("ACPUserUI: Channel closed when sending update");
                UIError::IOError(std::io::Error::other("Channel closed"))
            })?;

        // Wait for acknowledgment
        rx.await.map_err(|_| {
            tracing::error!("ACPUserUI: Failed to receive acknowledgment");
            UIError::IOError(std::io::Error::other("Failed to receive ack"))
        })?;

        tracing::debug!("ACPUserUI: Update sent and acknowledged");
        Ok(())
    }

    fn update_tool_call<F>(&self, tool_id: &str, updater: F) -> acp::ToolCallUpdate
    where
        F: FnOnce(&mut ToolCallState),
    {
        let tool_id = tool_id.to_string();
        let update = {
            let mut tool_calls = self.tool_calls.lock().unwrap();
            let state = tool_calls
                .entry(tool_id.clone())
                .or_insert_with(|| ToolCallState::new(&tool_id));
            updater(state);
            state.to_update()
        };
        update
    }

    fn get_tool_call<F>(&self, tool_id: &str, mutator: F) -> acp::ToolCall
    where
        F: FnOnce(&mut ToolCallState),
    {
        let tool_id = tool_id.to_string();
        let tool_call = {
            let mut tool_calls = self.tool_calls.lock().unwrap();
            let state = tool_calls
                .entry(tool_id.clone())
                .or_insert_with(|| ToolCallState::new(&tool_id));
            mutator(state);
            state.to_tool_call()
        };
        tool_call
    }

    fn queue_session_update(&self, update: acp::SessionUpdate) {
        let (ack_tx, _ack_rx) = oneshot::channel();
        let notification = acp::SessionNotification {
            session_id: self.session_id.clone(),
            update,
        };

        if let Err(e) = self.session_update_tx.send((notification, ack_tx)) {
            tracing::error!("ACPUserUI: Failed to send queued update: {:?}", e);
        } else {
            tracing::trace!("ACPUserUI: Queued session update");
        }
    }
}

#[async_trait]
impl UserInterface for ACPUserUI {
    async fn send_event(&self, event: UiEvent) -> Result<(), UIError> {
        match event {
            UiEvent::DisplayUserInput {
                content,
                attachments,
            } => {
                // Send user message content
                self.send_session_update(acp::SessionUpdate::UserMessageChunk {
                    content: acp::ContentBlock::Text(acp::TextContent {
                        annotations: None,
                        text: content,
                    }),
                })
                .await?;

                // Send attachments as additional content blocks
                for attachment in attachments {
                    #[allow(clippy::single_match)]
                    match attachment {
                        crate::persistence::DraftAttachment::Image { content, mime_type } => {
                            self.send_session_update(acp::SessionUpdate::UserMessageChunk {
                                content: acp::ContentBlock::Image(acp::ImageContent {
                                    annotations: None,
                                    data: content,
                                    mime_type,
                                    uri: None,
                                }),
                            })
                            .await?;
                        }
                        _ => {} // Ignore other attachment types for now
                    }
                }
            }

            UiEvent::UpdateToolStatus {
                tool_id,
                status,
                message,
                output,
            } => {
                let tool_status = map_tool_status(status);
                let message_clone = message.clone();
                let output_clone = output.clone();
                let tool_call_update = self.update_tool_call(&tool_id, |state| {
                    state.update_status(tool_status, message_clone.clone(), output_clone.clone());
                });
                self.send_session_update(acp::SessionUpdate::ToolCallUpdate(tool_call_update))
                    .await?;
            }

            UiEvent::AppendToTextBlock { .. }
            | UiEvent::AppendToThinkingBlock { .. }
            | UiEvent::StartTool { .. }
            | UiEvent::UpdateToolParameter { .. }
            | UiEvent::EndTool { .. }
            | UiEvent::AddImage { .. }
            | UiEvent::AppendToolOutput { .. }
            | UiEvent::StartReasoningSummaryItem
            | UiEvent::AppendReasoningSummaryDelta { .. }
            | UiEvent::CompleteReasoning => {
                tracing::trace!(
                    "ACPUserUI: streaming event received via send_event; handled via display_fragment"
                );
            }

            // Events that don't translate to ACP
            UiEvent::UpdateMemory { .. }
            | UiEvent::SetMessages { .. }
            | UiEvent::StreamingStarted(_)
            | UiEvent::StreamingStopped { .. }
            | UiEvent::RefreshChatList
            | UiEvent::UpdateChatList { .. }
            | UiEvent::ClearMessages
            | UiEvent::SendUserMessage { .. }
            | UiEvent::UpdateSessionMetadata { .. }
            | UiEvent::UpdateSessionActivityState { .. }
            | UiEvent::QueueUserMessage { .. }
            | UiEvent::RequestPendingMessageEdit { .. }
            | UiEvent::UpdatePendingMessage { .. }
            | UiEvent::DisplayError { .. }
            | UiEvent::ClearError => {
                // These are UI management events, not relevant for ACP
            }
        }
        Ok(())
    }

    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError> {
        match fragment {
            DisplayFragment::PlainText(_)
            | DisplayFragment::ThinkingText(_)
            | DisplayFragment::Image { .. } => {
                let content = fragment_to_content_block(fragment);
                self.queue_session_update(acp::SessionUpdate::AgentMessageChunk { content });
            }
            DisplayFragment::ToolName { name, id } => {
                if id.is_empty() {
                    tracing::warn!(
                        "ACPUserUI: StreamingProcessor provided empty tool ID for tool '{}'",
                        name
                    );
                    return Err(UIError::IOError(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Empty tool ID for tool '{name}'"),
                    )));
                }

                let tool_call = self.get_tool_call(&id, |state| {
                    state.set_tool_name(name);
                });

                self.queue_session_update(acp::SessionUpdate::ToolCall(tool_call));
            }
            DisplayFragment::ToolParameter {
                name,
                value,
                tool_id,
            } => {
                if tool_id.is_empty() {
                    tracing::warn!(
                        "ACPUserUI: StreamingProcessor provided empty tool ID for parameter '{}'",
                        name
                    );
                    return Err(UIError::IOError(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Empty tool ID for parameter '{name}'"),
                    )));
                }

                let name = name.clone();
                let value = value.clone();
                let tool_call_update = self.update_tool_call(&tool_id, |state| {
                    state.append_parameter(&name, &value);
                });

                self.queue_session_update(acp::SessionUpdate::ToolCallUpdate(tool_call_update));
            }
            DisplayFragment::ToolEnd { id } => {
                if id.is_empty() {
                    tracing::warn!(
                        "ACPUserUI: StreamingProcessor provided empty tool ID for ToolEnd"
                    );
                    return Err(UIError::IOError(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Empty tool ID for ToolEnd".to_string(),
                    )));
                }

                let tool_call_update = self.update_tool_call(&id, |state| {
                    state.ensure_completed();
                });

                self.queue_session_update(acp::SessionUpdate::ToolCallUpdate(tool_call_update));
            }
            DisplayFragment::ToolOutput { tool_id, chunk } => {
                if tool_id.is_empty() {
                    tracing::warn!(
                        "ACPUserUI: StreamingProcessor provided empty tool ID for ToolOutput"
                    );
                    return Err(UIError::IOError(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Empty tool ID for ToolOutput".to_string(),
                    )));
                }

                let chunk = chunk.clone();
                let tool_call_update = self.update_tool_call(&tool_id, |state| {
                    state.append_output_chunk(&chunk);
                });

                self.queue_session_update(acp::SessionUpdate::ToolCallUpdate(tool_call_update));
            }
            DisplayFragment::ReasoningSummaryStart | DisplayFragment::ReasoningComplete => {
                // No ACP representation needed yet
            }
            DisplayFragment::ReasoningSummaryDelta(delta) => {
                self.queue_session_update(acp::SessionUpdate::AgentMessageChunk {
                    content: acp::ContentBlock::Text(acp::TextContent {
                        annotations: None,
                        text: delta.clone(),
                    }),
                });
            }
        }
        Ok(())
    }

    fn should_streaming_continue(&self) -> bool {
        self.should_continue.load(Ordering::Relaxed)
    }

    fn notify_rate_limit(&self, _seconds_remaining: u64) {
        // Could send a custom meta field with rate limit info
    }

    fn clear_rate_limit(&self) {
        // No action needed
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
