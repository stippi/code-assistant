use agent_client_protocol as acp;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};

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
    async fn get_or_create_tool_call(&self, tool_id: &str, name: &str) -> acp::ToolCallUpdate {
        let mut tool_calls = self.tool_calls.lock().await;
        tool_calls
            .entry(tool_id.to_string())
            .or_insert_with(|| acp::ToolCallUpdate {
                id: acp::ToolCallId(tool_id.to_string().into()),
                fields: acp::ToolCallUpdateFields {
                    kind: Some(map_tool_kind(name)),
                    status: Some(acp::ToolCallStatus::Pending),
                    title: Some(name.to_string()),
                    content: Some(vec![]),
                    locations: None,
                    raw_input: None,
                    raw_output: None,
                },
            })
            .clone()
    }

    /// Update a tool call
    async fn update_tool_call<F>(&self, tool_id: &str, updater: F) -> acp::ToolCallUpdate
    where
        F: FnOnce(&mut acp::ToolCallUpdate),
    {
        let mut tool_calls = self.tool_calls.lock().await;
        if let Some(tool_call) = tool_calls.get_mut(tool_id) {
            updater(tool_call);
            tool_call.clone()
        } else {
            // Should not happen, but return a default
            acp::ToolCallUpdate {
                id: acp::ToolCallId(tool_id.to_string().into()),
                fields: acp::ToolCallUpdateFields {
                    kind: Some(acp::ToolKind::Other),
                    status: Some(acp::ToolCallStatus::Pending),
                    title: Some(String::new()),
                    content: Some(vec![]),
                    locations: None,
                    raw_input: None,
                    raw_output: None,
                },
            }
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
                let tool_call_update = self.get_or_create_tool_call(&id, &name).await;
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
                let tool_call_update = self
                    .update_tool_call(&tool_id, |tc| {
                        // Add or update parameter as text content
                        if let Some(ref mut content) = tc.fields.content {
                            content.push(acp::ToolCallContent::Content {
                                content: acp::ContentBlock::Text(acp::TextContent {
                                    annotations: None,
                                    text: format!("{name}: {value}"),
                                }),
                            });
                        }
                    })
                    .await;
                self.send_session_update(acp::SessionUpdate::ToolCallUpdate(tool_call_update))
                    .await?;
            }

            UiEvent::UpdateToolStatus {
                tool_id,
                status,
                message,
                output,
            } => {
                let tool_call_update = self
                    .update_tool_call(&tool_id, |tc| {
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
                    })
                    .await;
                self.send_session_update(acp::SessionUpdate::ToolCallUpdate(tool_call_update))
                    .await?;
            }

            UiEvent::EndTool { id } => {
                let tool_call_update = self
                    .update_tool_call(&id, |tc| {
                        if let Some(status) = &tc.fields.status {
                            if *status == acp::ToolCallStatus::Pending
                                || *status == acp::ToolCallStatus::InProgress
                            {
                                tc.fields.status = Some(acp::ToolCallStatus::Completed);
                            }
                        }
                    })
                    .await;
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
                let tool_call_update = self
                    .update_tool_call(&tool_id, |tc| {
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
                    })
                    .await;
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
        // For ACP, we convert fragments to content blocks
        // This is called during streaming
        match fragment {
            DisplayFragment::PlainText(_)
            | DisplayFragment::ThinkingText(_)
            | DisplayFragment::Image { .. } => {
                let content = fragment_to_content_block(fragment);
                let update = acp::SessionUpdate::AgentMessageChunk { content };

                // Send to unbounded channel (non-blocking)
                // The receiver task will process it asynchronously
                let (ack_tx, _ack_rx) = oneshot::channel();
                let notification = acp::SessionNotification {
                    session_id: self.session_id.clone(),
                    update,
                };

                match self.session_update_tx.send((notification, ack_tx)) {
                    Ok(_) => {
                        tracing::debug!("ACPUserUI: Fragment queued for sending");
                    }
                    Err(e) => {
                        tracing::error!("ACPUserUI: Failed to send to channel: {:?}", e);
                    }
                }
            }
            // Tool fragments are handled via UiEvent::StartTool, etc.
            _ => {
                tracing::trace!("ACPUserUI: Ignoring non-text fragment");
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
