use agent_client_protocol as acp;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

use crate::acp::types::{fragment_to_content_block, map_tool_kind, map_tool_status};
use crate::ui::{DisplayFragment, UIError, UiEvent, UserInterface};

/// UserInterface implementation that sends session/update notifications via ACP
pub struct ACPUserUI {
    session_id: acp::SessionId,
    session_update_tx: mpsc::UnboundedSender<(acp::SessionNotification, oneshot::Sender<()>)>,
    // Track tool calls for status updates
    tool_calls: Arc<Mutex<HashMap<String, acp::ToolCallUpdate>>>,
    // Track if we should continue streaming (atomic for lock-free access from sync callbacks)
    should_continue: Arc<AtomicBool>,
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

    /// Get or create a tool call update
    fn create_tool_call_update(tool_id: &str, name: Option<&str>) -> acp::ToolCallUpdate {
        let title = name.unwrap_or(tool_id);
        let kind = name.map(map_tool_kind).unwrap_or(acp::ToolKind::Other);
        acp::ToolCallUpdate {
            id: acp::ToolCallId(tool_id.to_string().into()),
            fields: acp::ToolCallUpdateFields {
                kind: Some(kind),
                status: Some(acp::ToolCallStatus::Pending),
                title: Some(title.to_string()),
                content: Some(vec![]),
                locations: None,
                raw_input: None,
                raw_output: None,
            },
        }
    }

    fn get_or_create_tool_call(&self, tool_id: &str, name: &str) -> acp::ToolCallUpdate {
        let mut tool_calls = self.tool_calls.lock().unwrap();
        tool_calls
            .entry(tool_id.to_string())
            .and_modify(|tc| {
                if tc
                    .fields
                    .title
                    .as_deref()
                    .map(|t| t.is_empty())
                    .unwrap_or(true)
                {
                    tc.fields.title = Some(name.to_string());
                }
                if tc.fields.kind.is_none() {
                    tc.fields.kind = Some(map_tool_kind(name));
                }
            })
            .or_insert_with(|| Self::create_tool_call_update(tool_id, Some(name)))
            .clone()
    }

    /// Update a tool call
    fn update_tool_call<F>(&self, tool_id: &str, updater: F) -> acp::ToolCallUpdate
    where
        F: FnOnce(&mut acp::ToolCallUpdate),
    {
        let mut tool_calls = self.tool_calls.lock().unwrap();
        if let Some(tool_call) = tool_calls.get_mut(tool_id) {
            updater(tool_call);
            tool_call.clone()
        } else {
            let mut default = Self::create_tool_call_update(tool_id, None);
            updater(&mut default);
            tool_calls.insert(tool_id.to_string(), default.clone());
            default
        }
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

            UiEvent::AppendToTextBlock { content } => {
                self.send_session_update(acp::SessionUpdate::AgentMessageChunk {
                    content: acp::ContentBlock::Text(acp::TextContent {
                        annotations: None,
                        text: content,
                    }),
                })
                .await?;
            }

            UiEvent::AppendToThinkingBlock { content } => {
                // Thinking text - just send as regular text
                self.send_session_update(acp::SessionUpdate::AgentMessageChunk {
                    content: acp::ContentBlock::Text(acp::TextContent {
                        annotations: None,
                        text: content,
                    }),
                })
                .await?;
            }

            UiEvent::StartTool { name, id } => {
                let tool_call_update = self.get_or_create_tool_call(&id, &name);
                // Convert ToolCallUpdate to ToolCall for the initial notification
                let tool_call = acp::ToolCall {
                    id: tool_call_update.id.clone(),
                    kind: tool_call_update
                        .fields
                        .kind
                        .clone()
                        .unwrap_or(acp::ToolKind::Other),
                    title: tool_call_update
                        .fields
                        .title
                        .clone()
                        .unwrap_or_else(|| name.clone()),
                    status: acp::ToolCallStatus::Pending,
                    content: vec![],
                    locations: vec![],
                    raw_input: None,
                    raw_output: None,
                };
                self.send_session_update(acp::SessionUpdate::ToolCall(tool_call))
                    .await?;
            }

            UiEvent::UpdateToolParameter {
                tool_id,
                name,
                value,
            } => {
                let tool_call_update = self.update_tool_call(&tool_id, |tc| {
                    // Add or update parameter as text content
                    if let Some(ref mut content) = tc.fields.content {
                        content.push(acp::ToolCallContent::Content {
                            content: acp::ContentBlock::Text(acp::TextContent {
                                annotations: None,
                                text: format!("{name}: {value}"),
                            }),
                        });
                    }
                });
                self.send_session_update(acp::SessionUpdate::ToolCallUpdate(tool_call_update))
                    .await?;
            }

            UiEvent::UpdateToolStatus {
                tool_id,
                status,
                message,
                output,
            } => {
                let tool_call_update = self.update_tool_call(&tool_id, |tc| {
                    tc.fields.status = Some(map_tool_status(status));
                    if let Some(msg) = message {
                        if let Some(ref mut content) = tc.fields.content {
                            content.push(acp::ToolCallContent::Content {
                                content: acp::ContentBlock::Text(acp::TextContent {
                                    annotations: None,
                                    text: msg,
                                }),
                            });
                        }
                    }
                    if let Some(out) = output {
                        if let Some(ref mut content) = tc.fields.content {
                            content.push(acp::ToolCallContent::Content {
                                content: acp::ContentBlock::Text(acp::TextContent {
                                    annotations: None,
                                    text: out,
                                }),
                            });
                        }
                    }
                });
                self.send_session_update(acp::SessionUpdate::ToolCallUpdate(tool_call_update))
                    .await?;
            }

            UiEvent::EndTool { id } => {
                let tool_call_update = self.update_tool_call(&id, |tc| {
                    if let Some(status) = &tc.fields.status {
                        if *status == acp::ToolCallStatus::Pending
                            || *status == acp::ToolCallStatus::InProgress
                        {
                            tc.fields.status = Some(acp::ToolCallStatus::Completed);
                        }
                    }
                });
                self.send_session_update(acp::SessionUpdate::ToolCallUpdate(tool_call_update))
                    .await?;
            }

            UiEvent::AddImage { media_type, data } => {
                self.send_session_update(acp::SessionUpdate::AgentMessageChunk {
                    content: acp::ContentBlock::Image(acp::ImageContent {
                        annotations: None,
                        data,
                        mime_type: media_type,
                        uri: None,
                    }),
                })
                .await?;
            }

            UiEvent::AppendToolOutput { tool_id, chunk } => {
                let tool_call_update = self.update_tool_call(&tool_id, |tc| {
                    if let Some(ref mut content) = tc.fields.content {
                        // Append to last text content or create new one
                        if let Some(acp::ToolCallContent::Content {
                            content: acp::ContentBlock::Text(ref mut text_content),
                        }) = content.last_mut()
                        {
                            text_content.text.push_str(&chunk);
                        } else {
                            content.push(acp::ToolCallContent::Content {
                                content: acp::ContentBlock::Text(acp::TextContent {
                                    annotations: None,
                                    text: chunk,
                                }),
                            });
                        }
                    }
                });
                self.send_session_update(acp::SessionUpdate::ToolCallUpdate(tool_call_update))
                    .await?;
            }

            UiEvent::StartReasoningSummaryItem => {
                // OpenAI reasoning - send as thinking text
                // For now, we'll just track this but not send anything
                // Actual content comes in AppendReasoningSummaryDelta
            }

            UiEvent::AppendReasoningSummaryDelta { delta } => {
                // Send reasoning delta as text
                self.send_session_update(acp::SessionUpdate::AgentMessageChunk {
                    content: acp::ContentBlock::Text(acp::TextContent {
                        annotations: None,
                        text: delta,
                    }),
                })
                .await?;
            }

            UiEvent::CompleteReasoning => {
                // Reasoning complete - no action needed
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

                let tool_call = {
                    let mut tool_calls = self.tool_calls.lock().unwrap();
                    let entry = tool_calls
                        .entry(id.clone())
                        .or_insert_with(|| Self::create_tool_call_update(id, Some(name)));
                    entry.fields.kind = Some(map_tool_kind(name));
                    entry.fields.title = Some(name.clone());
                    entry
                        .fields
                        .status
                        .get_or_insert(acp::ToolCallStatus::Pending);
                    entry.fields.content.get_or_insert_with(Vec::new);

                    acp::ToolCall {
                        id: entry.id.clone(),
                        title: entry.fields.title.clone().unwrap_or_else(|| name.clone()),
                        kind: entry.fields.kind.clone().unwrap_or(acp::ToolKind::Other),
                        status: entry.fields.status.unwrap_or(acp::ToolCallStatus::Pending),
                        content: entry.fields.content.clone().unwrap_or_default(),
                        locations: entry.fields.locations.clone().unwrap_or_default(),
                        raw_input: entry.fields.raw_input.clone(),
                        raw_output: entry.fields.raw_output.clone(),
                    }
                };

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

                let tool_call_update = {
                    let mut tool_calls = self.tool_calls.lock().unwrap();
                    let entry = tool_calls
                        .entry(tool_id.clone())
                        .or_insert_with(|| Self::create_tool_call_update(tool_id, None));
                    entry.fields.content.get_or_insert_with(Vec::new).push(
                        acp::ToolCallContent::Content {
                            content: acp::ContentBlock::Text(acp::TextContent {
                                annotations: None,
                                text: format!("{name}: {value}"),
                            }),
                        },
                    );
                    entry.clone()
                };

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

                let tool_call_update = {
                    let mut tool_calls = self.tool_calls.lock().unwrap();
                    let entry = tool_calls
                        .entry(id.clone())
                        .or_insert_with(|| Self::create_tool_call_update(id, None));

                    let status = entry.fields.status.unwrap_or(acp::ToolCallStatus::Pending);
                    if matches!(
                        status,
                        acp::ToolCallStatus::Pending | acp::ToolCallStatus::InProgress
                    ) {
                        entry.fields.status = Some(acp::ToolCallStatus::Completed);
                    }

                    entry.clone()
                };

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

                let tool_call_update = {
                    let mut tool_calls = self.tool_calls.lock().unwrap();
                    let entry = tool_calls
                        .entry(tool_id.clone())
                        .or_insert_with(|| Self::create_tool_call_update(tool_id, None));
                    let content = entry.fields.content.get_or_insert_with(Vec::new);
                    if let Some(acp::ToolCallContent::Content {
                        content: acp::ContentBlock::Text(text_content),
                    }) = content.last_mut()
                    {
                        text_content.text.push_str(chunk);
                    } else {
                        content.push(acp::ToolCallContent::Content {
                            content: acp::ContentBlock::Text(acp::TextContent {
                                annotations: None,
                                text: chunk.clone(),
                            }),
                        });
                    }
                    entry.clone()
                };

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
