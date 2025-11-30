use agent_client_protocol as acp;
use async_trait::async_trait;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::acp::types::{fragment_to_content_block, map_tool_kind, map_tool_status};
use crate::tools::core::registry::ToolRegistry;
use crate::ui::{DisplayFragment, UIError, UiEvent, UserInterface};

/// UserInterface implementation that sends session/update notifications via ACP
pub struct ACPUserUI {
    session_id: acp::SessionId,
    session_update_tx: mpsc::UnboundedSender<(acp::SessionNotification, oneshot::Sender<()>)>,
    // Track tool calls for status updates
    tool_calls: Arc<Mutex<HashMap<String, ToolCallState>>>,
    base_path: Option<PathBuf>,
    // Track if we should continue streaming (atomic for lock-free access from sync callbacks)
    should_continue: Arc<AtomicBool>,
    last_error: Arc<Mutex<Option<String>>>,
}

#[derive(Default, Clone)]
struct ParameterValue {
    value: String,
}

impl ParameterValue {
    fn append(&mut self, chunk: &str) {
        self.value.push_str(chunk);
    }

    fn replace(&mut self, value: &str) {
        self.value.clear();
        self.value.push_str(value);
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
    terminal_id: Option<acp::TerminalId>,
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
            terminal_id: None,
        }
    }

    fn set_tool_name(&mut self, name: &str) {
        self.tool_name = Some(name.to_string());
        self.title.get_or_insert_with(|| name.to_string());
        self.kind.get_or_insert_with(|| map_tool_kind(name));
    }

    fn kind(&self) -> acp::ToolKind {
        self.kind.unwrap_or(acp::ToolKind::Other)
    }

    fn status(&self) -> acp::ToolCallStatus {
        self.status
    }

    fn append_parameter(&mut self, name: &str, value: &str) {
        let entry = self.parameters.entry(name.to_string()).or_default();
        entry.append(value);

        // Update title if we have a template for this tool
        if let Some(tool_name) = &self.tool_name {
            let tool_name = tool_name.clone(); // Clone to avoid borrow issues
            self.update_title_from_template(&tool_name);
        }
    }

    fn replace_parameter(&mut self, name: &str, value: &str) {
        let entry = self.parameters.entry(name.to_string()).or_default();
        entry.replace(value);
        if let Some(tool_name) = &self.tool_name {
            let tool_name = tool_name.clone();
            self.update_title_from_template(&tool_name);
        }
    }

    fn update_title_from_template(&mut self, tool_name: &str) {
        let registry = ToolRegistry::global();
        if let Some(tool) = registry.get(tool_name) {
            let spec = tool.spec();
            if let Some(template) = spec.title_template {
                if let Some(new_title) = self.generate_title_from_template(template) {
                    self.title = Some(new_title);
                }
            }
        }
    }

    fn generate_title_from_template(&self, template: &str) -> Option<String> {
        let mut result = template.to_string();
        let mut has_substitution = false;

        // Find all {parameter_name} patterns and replace them
        let re = regex::Regex::new(r"\{([^}]+)\}").ok()?;

        result = re
            .replace_all(&result, |caps: &regex::Captures| {
                let param_name = &caps[1];
                if let Some(param_value) = self.parameters.get(param_name) {
                    let formatted_value = self.format_parameter_for_title(&param_value.value);
                    if !formatted_value.trim().is_empty() {
                        has_substitution = true;
                        formatted_value
                    } else {
                        caps[0].to_string() // Keep placeholder if value is empty
                    }
                } else {
                    caps[0].to_string() // Keep placeholder if parameter not found
                }
            })
            .to_string();

        // Only return the new title if we actually made substitutions
        if has_substitution {
            Some(result)
        } else {
            None
        }
    }

    fn format_parameter_for_title(&self, value: &str) -> String {
        const MAX_TITLE_LENGTH: usize = 50;

        let trimmed = value.trim();
        if trimmed.is_empty() {
            return String::new();
        }

        // Try to parse as JSON and extract meaningful parts
        if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            match json_val {
                serde_json::Value::Array(arr) if !arr.is_empty() => {
                    let first = arr[0].as_str().unwrap_or("...").to_string();
                    if arr.len() > 1 {
                        format!("{} and {} more", first, arr.len() - 1)
                    } else {
                        first
                    }
                }
                serde_json::Value::String(s) => s,
                _ => trimmed.to_string(),
            }
        } else {
            trimmed.to_string()
        }
        .chars()
        .take(MAX_TITLE_LENGTH)
        .collect::<String>()
            + if trimmed.len() > MAX_TITLE_LENGTH {
                "..."
            } else {
                ""
            }
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
        if self.terminal_id.is_some() {
            return;
        }
        if chunk.is_empty() {
            return;
        }
        self.output_stream
            .get_or_insert_with(String::new)
            .push_str(chunk);
    }

    fn set_terminal(&mut self, terminal_id: &str) {
        if terminal_id.is_empty() {
            return;
        }

        self.terminal_id = Some(acp::TerminalId(Arc::<str>::from(terminal_id.to_string())));
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
        self.output_text().map(JsonValue::String)
    }

    fn diff_content(&self, base_path: Option<&Path>) -> Option<acp::ToolCallContent> {
        if !matches!(
            self.tool_name.as_deref(),
            Some("edit") | Some("write_file") | Some("replace_in_file")
        ) {
            return None;
        }

        let path = self.parameters.get("path")?.value.trim();
        if path.is_empty() {
            return None;
        }
        let new_text = self.parameters.get("new_text")?.value.clone();
        let old_text = self.parameters.get("old_text").map(|v| v.value.clone());

        let diff = acp::Diff {
            path: resolve_path(path, base_path),
            old_text,
            new_text,
            meta: None,
        };

        Some(acp::ToolCallContent::Diff { diff })
    }

    fn build_content(&self, base_path: Option<&Path>) -> Option<Vec<acp::ToolCallContent>> {
        let mut content = Vec::new();
        let is_failed = matches!(self.status, acp::ToolCallStatus::Failed);

        // Always add terminal content first if present
        if let Some(terminal_id) = &self.terminal_id {
            content.push(acp::ToolCallContent::Terminal {
                terminal_id: terminal_id.clone(),
            });
        }

        // For file modification tools (edit, write_file, replace_in_file), use diff content
        if let Some(diff_content) = self.diff_content(base_path) {
            content.push(diff_content);
        } else if self.terminal_id.is_some() && !self.parameters.is_empty() {
            // For terminal tools, show parameters (like the command being run)
            let mut lines = Vec::new();
            for (name, value) in &self.parameters {
                lines.push(format!("{name}: {}", value.value));
            }
            content.push(text_content(lines.join("\n")));
        } else {
            // For all other tools, put the full output as the primary content
            if let Some(output) = self.output_text() {
                if !output.is_empty() {
                    content.push(text_content(output));
                }
            }
        }

        // Add error messages for failed tools
        if is_failed {
            if let Some(message) = &self.status_message {
                if !message.is_empty() {
                    // Only add status message if it's different from the output
                    let should_add_status = self
                        .output_text()
                        .map(|output| message.trim() != output.trim())
                        .unwrap_or(true);

                    if should_add_status {
                        content.push(text_content(message.clone()));
                    }
                }
            }
        }

        if content.is_empty() {
            None
        } else {
            Some(content)
        }
    }

    fn build_locations(&self, base_path: Option<&Path>) -> Option<Vec<acp::ToolCallLocation>> {
        let path_value = self.parameters.get("path")?.value.trim();
        if path_value.is_empty() {
            return None;
        }

        let resolved = resolve_path(path_value, base_path);

        let line = self
            .parameters
            .get("line")
            .or_else(|| self.parameters.get("line_number"))
            .and_then(|value| value.value.trim().parse::<u32>().ok());

        Some(vec![acp::ToolCallLocation {
            path: resolved,
            line,
            meta: None,
        }])
    }

    fn to_tool_call(&self, base_path: Option<&Path>) -> acp::ToolCall {
        acp::ToolCall {
            id: self.id.clone(),
            title: self
                .title
                .clone()
                .or_else(|| self.tool_name.clone())
                .unwrap_or_default(),
            kind: self.kind(),
            status: self.status(),
            content: self.build_content(base_path).unwrap_or_default(),
            locations: self.build_locations(base_path).unwrap_or_default(),
            raw_input: self.raw_input(),
            raw_output: self.raw_output(),
            meta: None,
        }
    }

    fn to_update(&self, base_path: Option<&Path>) -> acp::ToolCallUpdate {
        acp::ToolCallUpdate {
            id: self.id.clone(),
            meta: None,
            fields: acp::ToolCallUpdateFields {
                kind: self.kind,
                status: Some(self.status()),
                title: self.title.clone(),
                content: self.build_content(base_path),
                locations: self.build_locations(base_path),
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
            meta: None,
        }),
    }
}

fn resolve_path(path: &str, base_path: Option<&Path>) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        candidate
    } else if let Some(root) = base_path {
        root.join(candidate)
    } else {
        candidate
    }
}

impl ACPUserUI {
    pub fn new(
        session_id: acp::SessionId,
        session_update_tx: mpsc::UnboundedSender<(acp::SessionNotification, oneshot::Sender<()>)>,
        base_path: Option<PathBuf>,
    ) -> Self {
        Self {
            session_id,
            session_update_tx,
            tool_calls: Arc::new(Mutex::new(HashMap::new())),
            base_path,
            should_continue: Arc::new(AtomicBool::new(true)),
            last_error: Arc::new(Mutex::new(None)),
        }
    }

    /// Signal that the operation should be cancelled
    /// This is called by the cancel() method to stop the prompt() loop
    pub fn signal_cancel(&self) {
        self.should_continue.store(false, Ordering::Relaxed);
    }

    fn content_chunk(content: acp::ContentBlock) -> acp::ContentChunk {
        acp::ContentChunk {
            content,
            meta: None,
        }
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
                    meta: None,
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
        let base_path = self.base_path.as_deref();
        let update = {
            let mut tool_calls = self.tool_calls.lock().unwrap();
            let state = tool_calls
                .entry(tool_id.clone())
                .or_insert_with(|| ToolCallState::new(&tool_id));
            updater(state);
            state.to_update(base_path)
        };
        update
    }

    fn get_tool_call<F>(&self, tool_id: &str, mutator: F) -> acp::ToolCall
    where
        F: FnOnce(&mut ToolCallState),
    {
        let tool_id = tool_id.to_string();
        let base_path = self.base_path.as_deref();
        let tool_call = {
            let mut tool_calls = self.tool_calls.lock().unwrap();
            let state = tool_calls
                .entry(tool_id.clone())
                .or_insert_with(|| ToolCallState::new(&tool_id));
            mutator(state);
            state.to_tool_call(base_path)
        };
        tool_call
    }

    fn queue_session_update(&self, update: acp::SessionUpdate) {
        let (ack_tx, _ack_rx) = oneshot::channel();
        let notification = acp::SessionNotification {
            session_id: self.session_id.clone(),
            update,
            meta: None,
        };

        if let Err(e) = self.session_update_tx.send((notification, ack_tx)) {
            tracing::error!("ACPUserUI: Failed to send queued update: {:?}", e);
        } else {
            tracing::trace!("ACPUserUI: Queued session update");
        }
    }

    pub fn tool_call_update(&self, tool_id: &str) -> Option<acp::ToolCallUpdate> {
        let base_path = self.base_path.as_deref();
        let tool_calls = self.tool_calls.lock().unwrap();
        tool_calls
            .get(tool_id)
            .map(|state| state.to_update(base_path))
    }

    pub fn take_last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .ok()
            .and_then(|mut guard| guard.take())
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
                self.send_session_update(acp::SessionUpdate::UserMessageChunk(
                    Self::content_chunk(acp::ContentBlock::Text(acp::TextContent {
                        annotations: None,
                        text: content,
                        meta: None,
                    })),
                ))
                .await?;

                // Send attachments as additional content blocks
                for attachment in attachments {
                    #[allow(clippy::single_match)]
                    match attachment {
                        crate::persistence::DraftAttachment::Image { content, mime_type } => {
                            self.send_session_update(acp::SessionUpdate::UserMessageChunk(
                                Self::content_chunk(acp::ContentBlock::Image(acp::ImageContent {
                                    annotations: None,
                                    data: content,
                                    mime_type,
                                    uri: None,
                                    meta: None,
                                })),
                            ))
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

            UiEvent::UpdatePlan { plan } => {
                let entries = plan
                    .entries
                    .into_iter()
                    .map(|entry| acp::PlanEntry {
                        content: entry.content,
                        priority: match entry.priority {
                            crate::types::PlanItemPriority::High => acp::PlanEntryPriority::High,
                            crate::types::PlanItemPriority::Medium => {
                                acp::PlanEntryPriority::Medium
                            }
                            crate::types::PlanItemPriority::Low => acp::PlanEntryPriority::Low,
                        },
                        status: match entry.status {
                            crate::types::PlanItemStatus::Pending => acp::PlanEntryStatus::Pending,
                            crate::types::PlanItemStatus::InProgress => {
                                acp::PlanEntryStatus::InProgress
                            }
                            crate::types::PlanItemStatus::Completed => {
                                acp::PlanEntryStatus::Completed
                            }
                        },
                        meta: entry.meta,
                    })
                    .collect();

                let acp_plan = acp::Plan {
                    entries,
                    meta: plan.meta,
                };

                self.send_session_update(acp::SessionUpdate::Plan(acp_plan))
                    .await?;
            }

            UiEvent::AppendToTextBlock { .. }
            | UiEvent::AppendToThinkingBlock { .. }
            | UiEvent::StartTool { .. } => {
                tracing::trace!(
                    "ACPUserUI: streaming event received via send_event; handled via display_fragment"
                );
            }
            UiEvent::UpdateToolParameter {
                tool_id,
                name,
                value,
            } => {
                if tool_id.is_empty() {
                    tracing::warn!("ACPUserUI: UpdateToolParameter with empty tool_id");
                } else {
                    let name = name.clone();
                    let value = value.clone();
                    let tool_call_update = self.update_tool_call(&tool_id, |state| {
                        state.replace_parameter(&name, &value);
                    });
                    self.send_session_update(acp::SessionUpdate::ToolCallUpdate(tool_call_update))
                        .await?;
                }
            }
            UiEvent::EndTool { .. }
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
            | UiEvent::DisplayCompactionSummary { .. }
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
            | UiEvent::ClearError
            | UiEvent::UpdateCurrentModel { .. }
            | UiEvent::UpdateSandboxPolicy { .. } => {
                // These are UI management events, not relevant for ACP
            }
            UiEvent::DisplayError { message } => {
                tracing::error!("ACPUserUI: Received DisplayError event: {}", message);
                if let Ok(mut last_error) = self.last_error.lock() {
                    *last_error = Some(message);
                }
            }
        }
        Ok(())
    }

    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError> {
        match fragment {
            DisplayFragment::PlainText(_) | DisplayFragment::Image { .. } => {
                let content = fragment_to_content_block(fragment);
                let chunk = Self::content_chunk(content);
                self.queue_session_update(acp::SessionUpdate::AgentMessageChunk(chunk));
            }
            DisplayFragment::CompactionDivider { .. } => {
                let content = fragment_to_content_block(fragment);
                let chunk = Self::content_chunk(content);
                self.queue_session_update(acp::SessionUpdate::AgentMessageChunk(chunk));
            }
            DisplayFragment::ThinkingText(_) => {
                let content = fragment_to_content_block(fragment);
                let chunk = Self::content_chunk(content);
                self.queue_session_update(acp::SessionUpdate::AgentThoughtChunk(chunk));
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

                let tool_call = self.get_tool_call(id, |state| {
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
                let tool_call_update = self.update_tool_call(tool_id, |state| {
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

                let tool_call_update = self.update_tool_call(id, |state| {
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
                let tool_call_update = self.update_tool_call(tool_id, |state| {
                    state.append_output_chunk(&chunk);
                });

                self.queue_session_update(acp::SessionUpdate::ToolCallUpdate(tool_call_update));
            }
            DisplayFragment::ToolTerminal {
                tool_id,
                terminal_id,
            } => {
                if tool_id.is_empty() || terminal_id.is_empty() {
                    tracing::warn!(
                        "ACPUserUI: ToolTerminal fragment missing tool_id or terminal_id"
                    );
                    return Err(UIError::IOError(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "ToolTerminal fragment missing identifiers".to_string(),
                    )));
                }

                let terminal_id = terminal_id.clone();
                let tool_call_update = self.update_tool_call(tool_id, |state| {
                    state.set_terminal(&terminal_id);
                });

                self.queue_session_update(acp::SessionUpdate::ToolCallUpdate(tool_call_update));
            }
            DisplayFragment::ReasoningSummaryStart | DisplayFragment::ReasoningComplete => {
                // No ACP representation needed yet
            }
            DisplayFragment::ReasoningSummaryDelta(delta) => {
                // Reasoning summaries are emitted as AgentThoughtChunk, same as ThinkingText
                self.queue_session_update(acp::SessionUpdate::AgentThoughtChunk(
                    Self::content_chunk(acp::ContentBlock::Text(acp::TextContent {
                        annotations: None,
                        text: delta.clone(),
                        meta: None,
                    })),
                ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PlanItem, PlanItemPriority, PlanItemStatus, PlanState};
    use serde_json::json;
    use tokio::sync::{mpsc, oneshot};

    fn create_ui() -> (
        ACPUserUI,
        mpsc::UnboundedReceiver<(acp::SessionNotification, oneshot::Sender<()>)>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let ui = ACPUserUI::new(acp::SessionId("session-1".to_string().into()), tx, None);
        (ui, rx)
    }

    #[test]
    fn tool_call_state_includes_terminal_content() {
        let mut state = ToolCallState::new("tool-1");
        state.set_tool_name("execute_command");
        state.append_parameter("command", "npm test");
        state.append_output_chunk("running…\n");

        state.set_terminal("term-123");
        let content = state
            .build_content(None)
            .expect("content should be emitted");

        assert!(matches!(
            content.first(),
            Some(acp::ToolCallContent::Terminal { terminal_id })
                if terminal_id.0.as_ref() == "term-123"
        ));

        assert!(content.iter().any(|item| matches!(
            item,
            acp::ToolCallContent::Content {
                content: acp::ContentBlock::Text(acp::TextContent { text, .. })
            } if text.contains("command: npm test")
        )));
    }

    #[test]
    fn tool_output_stops_streaming_after_terminal_attached() {
        let mut state = ToolCallState::new("tool-1");
        state.append_output_chunk("line one\n");
        assert_eq!(state.output_text().as_deref(), Some("line one\n"));

        state.set_terminal("term-123");
        state.append_output_chunk("line two\n");

        assert_eq!(state.output_text().as_deref(), Some("line one\n"));
    }

    #[test]
    fn tool_name_fragment_emits_tool_call_notification() {
        let (ui, mut rx) = create_ui();

        ui.display_fragment(&DisplayFragment::ToolName {
            name: "execute_command".into(),
            id: "tool-1".into(),
        })
        .unwrap();

        let (notification, _ack) = rx.try_recv().expect("expected tool call notification");
        match notification.update {
            acp::SessionUpdate::ToolCall(call) => {
                assert_eq!(call.id.0.as_ref(), "tool-1");
                assert_eq!(call.kind, acp::ToolKind::Execute);
                assert_eq!(call.title, "execute_command");
            }
            other => panic!("unexpected update: {other:?}"),
        }
    }

    #[test]
    fn tool_output_fragments_accumulate_raw_output() {
        let (ui, mut rx) = create_ui();

        ui.display_fragment(&DisplayFragment::ToolName {
            name: "read_files".into(),
            id: "tool-1".into(),
        })
        .unwrap();
        rx.try_recv().unwrap(); // discard ToolCall notification

        ui.display_fragment(&DisplayFragment::ToolOutput {
            tool_id: "tool-1".into(),
            chunk: "part one".into(),
        })
        .unwrap();
        let (notification, _ack) = rx.try_recv().expect("first tool update");
        let update = match notification.update {
            acp::SessionUpdate::ToolCallUpdate(update) => update,
            other => panic!("unexpected update: {other:?}"),
        };
        assert_eq!(
            update
                .fields
                .raw_output
                .clone()
                .and_then(|value| value.as_str().map(str::to_owned)),
            Some("part one".to_string())
        );

        ui.display_fragment(&DisplayFragment::ToolOutput {
            tool_id: "tool-1".into(),
            chunk: "part two".into(),
        })
        .unwrap();
        let (notification, _ack) = rx.try_recv().expect("second tool update");
        let update = match notification.update {
            acp::SessionUpdate::ToolCallUpdate(update) => update,
            other => panic!("unexpected update: {other:?}"),
        };
        assert_eq!(
            update
                .fields
                .raw_output
                .and_then(|value| value.as_str().map(str::to_owned)),
            Some("part onepart two".to_string())
        );
    }

    #[tokio::test]
    async fn send_event_streams_user_message() {
        let (ui, mut rx) = create_ui();

        let send_future = ui.send_event(UiEvent::DisplayUserInput {
            content: "Hello".into(),
            attachments: vec![],
        });
        let receive_future = async {
            let (notification, ack) = rx.recv().await.expect("session update");
            ack.send(()).unwrap();
            notification
        };

        let (send_result, notification) = tokio::join!(send_future, receive_future);
        send_result.unwrap();

        match notification.update {
            acp::SessionUpdate::UserMessageChunk(chunk) => match chunk.content {
                acp::ContentBlock::Text(text) => assert_eq!(text.text, "Hello"),
                other => panic!("unexpected content: {other:?}"),
            },
            other => panic!("unexpected update: {other:?}"),
        }
    }

    #[test]
    fn tool_call_content_prioritizes_output_over_parameters() {
        let mut state = ToolCallState::new("tool-1");
        state.set_tool_name("read_files");
        state.append_parameter("paths", "[\"file1.txt\", \"file2.txt\"]");
        state.append_output_chunk("Successfully loaded the following file(s):\n");
        state.append_output_chunk(">>>>> FILE: file1.txt\nContent of file 1\n");
        state.append_output_chunk(">>>>> FILE: file2.txt\nContent of file 2\n");

        let content = state
            .build_content(None)
            .expect("content should be emitted");

        // Should contain the full output, not just parameters
        assert_eq!(content.len(), 1);
        match &content[0] {
            acp::ToolCallContent::Content {
                content: acp::ContentBlock::Text(acp::TextContent { text, .. }),
            } => {
                assert!(text.contains("Successfully loaded the following file(s)"));
                assert!(text.contains("Content of file 1"));
                assert!(text.contains("Content of file 2"));
                assert!(!text.contains("paths: [\"file1.txt\", \"file2.txt\"]"));
            }
            other => panic!("expected text content, got {other:?}"),
        }
    }

    #[test]
    fn edit_tool_still_uses_diff_content() {
        let mut state = ToolCallState::new("tool-1");
        state.set_tool_name("edit");
        state.append_parameter("path", "test.txt");
        state.append_parameter("old_text", "old content");
        state.append_parameter("new_text", "new content");
        state.append_output_chunk("File edited successfully");

        let content = state
            .build_content(None)
            .expect("content should be emitted");

        // Should contain diff content, not output
        assert_eq!(content.len(), 1);
        match &content[0] {
            acp::ToolCallContent::Diff { diff } => {
                assert_eq!(diff.path.to_string_lossy(), "test.txt");
                assert_eq!(diff.old_text.as_deref(), Some("old content"));
                assert_eq!(diff.new_text, "new content");
            }
            other => panic!("expected diff content, got {other:?}"),
        }
    }

    #[test]
    fn tool_title_updates_from_template() {
        let mut state = ToolCallState::new("tool-1");

        // Initially should have no title
        assert_eq!(state.title, None);

        // Set tool name - should get default title
        state.set_tool_name("read_files");
        assert_eq!(state.title, Some("read_files".to_string()));

        // Add parameter that should update title
        state.append_parameter("paths", r#"["src/main.rs", "src/lib.rs"]"#);

        // Title should now be updated with the paths
        assert!(state.title.as_ref().unwrap().contains("src/main.rs"));
        assert!(state.title.as_ref().unwrap().starts_with("Reading"));
    }

    #[test]
    fn tool_title_handles_streaming_parameters() {
        let mut state = ToolCallState::new("tool-1");
        state.set_tool_name("search_files");

        // Add parameter in chunks (simulating streaming)
        state.append_parameter("regex", "fn");
        state.append_parameter("regex", " main");
        state.append_parameter("regex", "\\(");

        // Should have meaningful title even with partial parameter
        if let Some(title) = &state.title {
            assert!(title.contains("fn main\\("));
            assert!(title.starts_with("Searching for"));
        }
    }

    #[test]
    fn tool_title_formatting_handles_json_arrays() {
        let mut state = ToolCallState::new("tool-1");
        state.set_tool_name("read_files");

        // Add JSON array parameter
        state.append_parameter("paths", r#"["file1.txt", "file2.txt", "file3.txt"]"#);

        // Should format array nicely
        if let Some(title) = &state.title {
            assert!(title.contains("file1.txt and 2 more") || title.contains("file1.txt"));
            assert!(title.starts_with("Reading"));
        }
    }

    #[tokio::test]
    async fn send_event_emits_plan_update() {
        let (ui, mut rx) = create_ui();

        let plan = PlanState {
            entries: vec![
                PlanItem {
                    content: "Investigate plan bridge".into(),
                    priority: PlanItemPriority::High,
                    status: PlanItemStatus::InProgress,
                    meta: Some(json!({"ticket": 42})),
                },
                PlanItem {
                    content: "Write ACP plan test".into(),
                    priority: PlanItemPriority::Low,
                    status: PlanItemStatus::Completed,
                    meta: None,
                },
            ],
            meta: Some(json!({"source": "unit-test"})),
        };
        let expected_plan = plan.clone();

        let send_future = ui.send_event(UiEvent::UpdatePlan { plan });
        let receive_future = async {
            let (notification, ack) = rx.recv().await.expect("plan update");
            ack.send(()).unwrap();
            notification
        };

        let (send_result, notification) = tokio::join!(send_future, receive_future);
        send_result.unwrap();

        let acp::SessionUpdate::Plan(acp_plan) = notification.update else {
            panic!("expected plan update, got {:?}", notification.update);
        };

        assert_eq!(acp_plan.meta, expected_plan.meta);
        assert_eq!(acp_plan.entries.len(), expected_plan.entries.len());

        let first = &acp_plan.entries[0];
        assert_eq!(first.content, expected_plan.entries[0].content);
        assert_eq!(first.priority, acp::PlanEntryPriority::High);
        assert_eq!(first.status, acp::PlanEntryStatus::InProgress);
        assert_eq!(first.meta, expected_plan.entries[0].meta);

        let second = &acp_plan.entries[1];
        assert_eq!(second.content, expected_plan.entries[1].content);
        assert_eq!(second.priority, acp::PlanEntryPriority::Low);
        assert_eq!(second.status, acp::PlanEntryStatus::Completed);
        assert_eq!(second.meta, expected_plan.entries[1].meta);
    }
}
