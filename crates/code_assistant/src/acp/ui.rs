use agent_client_protocol as acp;
use async_trait::async_trait;
use std::collections::HashMap;
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
    // Track if we should continue streaming
    should_continue: Arc<Mutex<bool>>,
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
            should_continue: Arc::new(Mutex::new(true)),
        }
    }

    /// Send a session update notification
    async fn send_session_update(&self, update: acp::SessionUpdate) -> Result<(), UIError> {
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
                UIError::IOError(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Channel closed",
                ))
            })?;

        // Wait for acknowledgment
        rx.await.map_err(|_| {
            UIError::IOError(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Failed to receive ack",
            ))
        })?;

        Ok(())
    }

    /// Get or create a tool call update
    async fn get_or_create_tool_call(&self, tool_id: &str, name: &str) -> acp::ToolCallUpdate {
        let mut tool_calls = self.tool_calls.lock().await;
        tool_calls
            .entry(tool_id.to_string())
            .or_insert_with(|| acp::ToolCallUpdate {
                tool_call_id: acp::ToolCallId(tool_id.to_string().into()),
                title: name.to_string(),
                kind: map_tool_kind(name),
                status: acp::ToolCallStatus::Pending,
                content: vec![],
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
                tool_call_id: acp::ToolCallId(tool_id.to_string().into()),
                title: String::new(),
                kind: acp::ToolKind::Other,
                status: acp::ToolCallStatus::Pending,
                content: vec![],
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
                    match attachment {
                        crate::persistence::DraftAttachment::Image { data, media_type } => {
                            self.send_session_update(acp::SessionUpdate::UserMessageChunk {
                                content: acp::ContentBlock::Image(acp::ImageContent {
                                    annotations: None,
                                    data,
                                    mime_type: media_type,
                                    uri: None,
                                }),
                            })
                            .await?;
                        }
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
                let tool_call = self.get_or_create_tool_call(&id, &name).await;
                self.send_session_update(acp::SessionUpdate::ToolCall { update: tool_call })
                    .await?;
            }

            UiEvent::UpdateToolParameter {
                tool_id,
                name,
                value,
            } => {
                let tool_call = self
                    .update_tool_call(&tool_id, |tc| {
                        // Add or update parameter as text content
                        tc.content.push(acp::ToolCallContent::Text {
                            text: format!("{}: {}", name, value),
                        });
                    })
                    .await;
                self.send_session_update(acp::SessionUpdate::ToolCall { update: tool_call })
                    .await?;
            }

            UiEvent::UpdateToolStatus {
                tool_id,
                status,
                message,
                output,
            } => {
                let tool_call = self
                    .update_tool_call(&tool_id, |tc| {
                        tc.status = map_tool_status(status);
                        if let Some(msg) = message {
                            tc.content.push(acp::ToolCallContent::Text { text: msg });
                        }
                        if let Some(out) = output {
                            tc.content.push(acp::ToolCallContent::Terminal {
                                block: acp::TerminalBlock {
                                    output: out,
                                    exit_code: None,
                                },
                            });
                        }
                    })
                    .await;
                self.send_session_update(acp::SessionUpdate::ToolCall { update: tool_call })
                    .await?;
            }

            UiEvent::EndTool { id } => {
                let tool_call = self
                    .update_tool_call(&id, |tc| {
                        if tc.status == acp::ToolCallStatus::Pending
                            || tc.status == acp::ToolCallStatus::Executing
                        {
                            tc.status = acp::ToolCallStatus::Executed;
                        }
                    })
                    .await;
                self.send_session_update(acp::SessionUpdate::ToolCall { update: tool_call })
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
                let tool_call = self
                    .update_tool_call(&tool_id, |tc| {
                        // Append to last terminal block or create new one
                        if let Some(acp::ToolCallContent::Terminal { block }) =
                            tc.content.last_mut()
                        {
                            block.output.push_str(&chunk);
                        } else {
                            tc.content.push(acp::ToolCallContent::Terminal {
                                block: acp::TerminalBlock {
                                    output: chunk,
                                    exit_code: None,
                                },
                            });
                        }
                    })
                    .await;
                self.send_session_update(acp::SessionUpdate::ToolCall { update: tool_call })
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

                // We need to send this synchronously, but display_fragment is not async
                // We'll spawn a task to send it
                let tx = self.session_update_tx.clone();
                let session_id = self.session_id.clone();
                tokio::spawn(async move {
                    let (ack_tx, _ack_rx) = oneshot::channel();
                    let _ = tx.send((acp::SessionNotification { session_id, update }, ack_tx));
                });
            }
            // Tool fragments are handled via UiEvent::StartTool, etc.
            _ => {}
        }
        Ok(())
    }

    fn should_streaming_continue(&self) -> bool {
        *self.should_continue.blocking_lock()
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
