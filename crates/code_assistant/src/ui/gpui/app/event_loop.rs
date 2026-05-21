//! UI event processing loop.
//!
//! Contains `Gpui::process_ui_event_async` — the central event dispatcher
//! that translates `UiEvent`s into mutations on the message queue, sidebar,
//! and other UI state — and `process_fragments_for_container`.

use crate::ui::{DisplayFragment, UiEvent};

use super::super::blocks::{MessageContainer, MessageRole};
use gpui::Entity;
use tracing::{debug, trace, warn};

use super::super::*;

impl Gpui {
    pub(in crate::ui::gpui) fn process_ui_event_async(
        &self,
        event: UiEvent,
        cx: &mut gpui::AsyncApp,
    ) {
        match event {
            UiEvent::DisplayUserInput {
                content,
                attachments,
                node_id,
            } => {
                let old_len;
                {
                    let mut queue = self.message_queue.lock().unwrap();
                    old_len = queue.len();
                    let sid = self.current_session_id.lock().unwrap().clone();
                    let new_message = cx.new(|cx| {
                        let new_message = MessageContainer::with_role(MessageRole::User, cx);
                        new_message.set_session_id(sid);

                        // Set node_id for edit button support
                        new_message.set_node_id(node_id);

                        // Add text content if not empty
                        if !content.is_empty() {
                            new_message.add_text_block(&content, cx);
                        }

                        // Add attachments
                        for attachment in attachments {
                            match attachment {
                                crate::persistence::DraftAttachment::Image {
                                    content,
                                    mime_type,
                                    ..
                                } => {
                                    new_message.add_image_block(&mime_type, &content, cx);
                                }
                                crate::persistence::DraftAttachment::Text { content } => {
                                    new_message.add_text_block(&content, cx);
                                }
                                crate::persistence::DraftAttachment::File {
                                    content,
                                    filename,
                                    ..
                                } => {
                                    let file_text = format!("File: {filename}\n{content}");
                                    new_message.add_text_block(&file_text, cx);
                                }
                            }
                        }

                        new_message
                    });
                    queue.push(new_message);
                }

                // Sync ListState and reset pending message
                self.notify_messages_appended(old_len, cx);
                self.update_messages_view(cx, |messages_view, _cx| {
                    messages_view.update_pending_message(None);
                });
            }
            UiEvent::DisplayCompactionSummary { summary } => {
                let old_len;
                {
                    let mut queue = self.message_queue.lock().unwrap();
                    old_len = queue.len();
                    let sid = self.current_session_id.lock().unwrap().clone();
                    let new_message = cx.new(|cx| {
                        let message = MessageContainer::with_role(MessageRole::System, cx);
                        message.set_session_id(sid);
                        message.add_compaction_divider(summary.clone(), cx);
                        message
                    });
                    queue.push(new_message);
                }
                self.notify_messages_appended(old_len, cx);
            }

            UiEvent::AppendToTextBlock { content } => {
                // Since StreamingStarted ensures last container is Assistant, we can safely append
                self.update_last_message(cx, |message, cx| {
                    message.add_or_append_to_text_block(&content, cx)
                });
                self.auto_scroll_if_following(cx);
            }
            UiEvent::AppendToThinkingBlock { content } => {
                // Since StreamingStarted ensures last container is Assistant, we can safely append
                self.update_last_message(cx, |message, cx| {
                    message.add_or_append_to_thinking_block(&content, cx)
                });
                self.auto_scroll_if_following(cx);
            }
            UiEvent::StartTool { name, id } => {
                // Since StreamingStarted ensures last container is Assistant, we can safely add tool
                self.update_last_message(cx, |message, cx| {
                    message.add_tool_use_block(&name, &id, cx);
                });
                self.auto_scroll_if_following(cx);
            }

            UiEvent::UpdateToolParameter {
                tool_id,
                name,
                value,
                replace,
            } => {
                if replace {
                    warn!(
                        "GPUI event: replace tool parameter for tool_id='{}', param='{}', value_len={}",
                        tool_id,
                        name,
                        value.len()
                    );
                    self.update_all_messages(cx, |message, cx| {
                        message.replace_tool_parameter(&tool_id, &name, &value, cx);
                    });
                } else {
                    self.update_last_message(cx, |message, cx| {
                        message.add_or_update_tool_parameter(&tool_id, &name, &value, cx);
                    });
                }
                self.auto_scroll_if_following(cx);
            }

            UiEvent::UpdateToolStatus {
                tool_id,
                status,
                message,
                output,
                styled_output,
                duration_seconds,
                images,
            } => {
                // If the event doesn't carry styled output, check the cache
                // (populated by terminal_executor just before PTY cleanup).
                let styled_output = styled_output
                    .or_else(|| terminal_executor::take_cached_styled_output(&tool_id));
                // Convert ImageData to (media_type, base64_data) tuples for the UI
                let ui_images: Vec<(String, String)> = images
                    .iter()
                    .map(|img| (img.media_type.clone(), img.base64_data.clone()))
                    .collect();
                self.update_all_messages(cx, |message_container, cx| {
                    message_container.update_tool_status(
                        &tool_id,
                        status,
                        message.clone(),
                        output.clone(),
                        styled_output.clone(),
                        duration_seconds,
                        ui_images.clone(),
                        cx,
                    );
                });
                self.auto_scroll_if_following(cx);
            }

            UiEvent::EndTool { id } => {
                self.update_all_messages(cx, |message_container, cx| {
                    message_container.end_tool_use(&id, cx);
                });
                self.auto_scroll_if_following(cx);
            }
            UiEvent::HiddenToolCompleted => {
                // Mark that a hidden tool completed - message container handles paragraph breaks
                self.update_last_message(cx, |message, cx| {
                    message.mark_hidden_tool_completed(cx);
                });
                self.auto_scroll_if_following(cx);
            }

            UiEvent::UpdatePlan { plan } => {
                if let Ok(mut plan_guard) = self.plan_state.lock() {
                    *plan_guard = Some(plan);
                }
                cx.refresh();
            }
            UiEvent::SetMessages {
                messages,
                session_id,
                tool_results,
            } => {
                // Update current session ID if provided
                if let Some(ref session_id) = session_id {
                    *self.current_session_id.lock().unwrap() = Some(session_id.clone());
                    // Reset activity state when switching sessions - it will be updated by subsequent events
                    *self.current_session_activity_state.lock().unwrap() = None;

                    // Clear any stop request for the new session to start fresh
                    self.session_stop_requests
                        .lock()
                        .unwrap()
                        .remove(session_id);

                    // Find the current project for this session and update MessagesView
                    let current_project = {
                        let sessions = self.chat_sessions.lock().unwrap();
                        sessions
                            .iter()
                            .find(|s| s.id == *session_id)
                            .map(|s| s.initial_project.clone())
                            .unwrap_or_else(String::new)
                    };

                    warn!("Using initial project: '{}'", current_project);

                    // Update MessagesView with current project and session ID

                    let session_id_for_messages = session_id.clone();
                    self.update_messages_view(cx, |messages_view, _cx| {
                        messages_view.set_current_project(current_project.clone());
                        messages_view.set_current_session_id(Some(session_id_for_messages));
                    });
                }

                // Clear existing messages
                {
                    let mut queue = self.message_queue.lock().unwrap();
                    queue.clear();
                }

                // Get current project for new containers
                let current_project = if let Some(ref session_id) = session_id {
                    let sessions = self.chat_sessions.lock().unwrap();
                    sessions
                        .iter()
                        .find(|s| s.id == *session_id)
                        .map(|s| s.initial_project.clone())
                        .unwrap_or_else(String::new)
                } else {
                    String::new()
                };

                // Process message data — each MessageData gets its own container.
                // This ensures the virtual list has fine-grained items (one per
                // LLM request/node) so that GPUI's list virtualization can skip
                // off-screen items efficiently.
                for message_data in messages {
                    let current_container = {
                        let mut queue = self.message_queue.lock().unwrap();

                        // Always create a new container per message (each MessageData
                        // corresponds to one persisted node / LLM request).
                        let container =
                            cx.new(|cx| MessageContainer::with_role(message_data.role.clone(), cx));

                        let node_id = message_data.node_id;
                        let branch_info = message_data.branch_info.clone();
                        let sid = session_id.clone();
                        self.update_container(&container, cx, |container, _cx| {
                            container.set_current_project(current_project.clone());
                            container.set_node_id(node_id);
                            container.set_branch_info(branch_info);
                            container.set_session_id(sid);
                        });

                        queue.push(container.clone());
                        container
                    }; // Lock is released here

                    // Process fragments into the current container
                    self.process_fragments_for_container(
                        &current_container,
                        message_data.fragments,
                        cx,
                    );
                }

                // Apply tool results to update tool blocks with their execution results
                for tool_result in tool_results {
                    let ui_images: Vec<(String, String)> = tool_result
                        .images
                        .iter()
                        .map(|img| (img.media_type.clone(), img.base64_data.clone()))
                        .collect();

                    self.update_all_messages(cx, |message_container, cx| {
                        message_container.update_tool_status(
                            &tool_result.tool_id,
                            tool_result.status,
                            tool_result.message.clone(),
                            tool_result.output.clone(),
                            tool_result.styled_output.clone(),
                            tool_result.duration_seconds,
                            ui_images.clone(),
                            cx,
                        );
                    });
                }

                // Ensure we always end with an Assistant container
                // This is crucial for sessions that are waiting for responses or actively running agents
                {
                    let mut queue = self.message_queue.lock().unwrap();
                    let needs_assistant_container = if let Some(last) = queue.last() {
                        cx.update_entity(last, |message, _cx| message.is_user_message())
                    } else {
                        true // Empty queue needs an assistant container
                    };

                    if needs_assistant_container {
                        let assistant_container =
                            cx.new(|cx| MessageContainer::with_role(MessageRole::Assistant, cx));
                        let sid = session_id.clone();
                        self.update_container(&assistant_container, cx, |c, _cx| {
                            c.set_session_id(sid);
                        });
                        queue.push(assistant_container);
                    }
                }

                self.notify_messages_reset(cx);
            }

            UiEvent::AppendMessages {
                messages,
                tool_results,
            } => {
                // Incremental update: append new messages without clearing existing ones.
                // Deduplicate by node_id to avoid double-appending messages that the UI
                // already has (e.g. due to race between agent Idle transition and
                // file-watcher debounce — the streaming container already has the
                // pre-allocated node_id).
                let existing_node_ids: std::collections::HashSet<crate::persistence::NodeId> = {
                    let queue = self.message_queue.lock().unwrap();
                    queue
                        .iter()
                        .filter_map(|container| cx.update_entity(container, |c, _cx| c.node_id()))
                        .collect()
                };

                let messages: Vec<_> = messages
                    .into_iter()
                    .filter(|msg| match msg.node_id {
                        Some(id) if existing_node_ids.contains(&id) => {
                            debug!("AppendMessages: skipping duplicate node_id {}", id);
                            false
                        }
                        _ => true,
                    })
                    .collect();
                if messages.is_empty() && tool_results.is_empty() {
                    return;
                }

                let old_len = self.message_queue.lock().unwrap().len();

                let current_project = {
                    let sid = self.current_session_id.lock().unwrap().clone();
                    if let Some(ref session_id) = sid {
                        let sessions = self.chat_sessions.lock().unwrap();
                        sessions
                            .iter()
                            .find(|s| s.id == *session_id)
                            .map(|s| s.initial_project.clone())
                            .unwrap_or_default()
                    } else {
                        String::new()
                    }
                };

                let session_id = self.current_session_id.lock().unwrap().clone();

                for message_data in messages {
                    let current_container = {
                        let mut queue = self.message_queue.lock().unwrap();

                        // Always create a new container per message (each MessageData
                        // corresponds to one persisted node / LLM request).
                        let container =
                            cx.new(|cx| MessageContainer::with_role(message_data.role.clone(), cx));

                        let node_id = message_data.node_id;
                        let branch_info = message_data.branch_info.clone();
                        let sid = session_id.clone();
                        self.update_container(&container, cx, |container, _cx| {
                            container.set_current_project(current_project.clone());
                            container.set_node_id(node_id);
                            container.set_branch_info(branch_info);
                            container.set_session_id(sid);
                        });

                        queue.push(container.clone());
                        container
                    };

                    self.process_fragments_for_container(
                        &current_container,
                        message_data.fragments,
                        cx,
                    );
                }

                // Apply tool results
                for tool_result in tool_results {
                    let ui_images: Vec<(String, String)> = tool_result
                        .images
                        .iter()
                        .map(|img| (img.media_type.clone(), img.base64_data.clone()))
                        .collect();

                    self.update_all_messages(cx, |message_container, cx| {
                        message_container.update_tool_status(
                            &tool_result.tool_id,
                            tool_result.status,
                            tool_result.message.clone(),
                            tool_result.output.clone(),
                            tool_result.styled_output.clone(),
                            tool_result.duration_seconds,
                            ui_images.clone(),
                            cx,
                        );
                    });
                }

                self.notify_messages_appended(old_len, cx);
            }

            UiEvent::StreamingStarted {
                request_id,
                node_id,
            } => {
                // Finish any open thinking blocks in the previous container.
                // After the restructuring (one container per LLM request), a
                // thinking block in container A would never be completed if the
                // next text/tool fragment arrives in a new container B, because
                // finish_any_thinking_blocks only operates within the same
                // container.
                self.update_last_message(cx, |message, cx| {
                    message.finish_any_thinking_blocks(cx);
                });

                let old_len;
                {
                    let mut queue = self.message_queue.lock().unwrap();
                    old_len = queue.len();

                    // Check if the last container is an empty assistant container
                    // that we can reuse (e.g. pre-allocated by SetMessages).
                    let reuse_last = queue.last().is_some_and(|last| {
                        cx.update_entity(last, |c, _cx| !c.is_user_message() && c.is_empty())
                    });

                    if reuse_last {
                        // Reuse the existing empty assistant container
                        let last = queue.last().unwrap().clone();
                        drop(queue);
                        self.update_container(&last, cx, |container, cx| {
                            container.set_current_request_id(request_id);
                            container.set_node_id(Some(node_id));
                            cx.notify();
                        });
                        return;
                    }

                    // Create a new assistant container for this streaming request.
                    // Each LLM request maps to one list item, enabling effective
                    // virtualized scrolling.
                    let sid = self.current_session_id.lock().unwrap().clone();
                    let assistant_container = cx.new(|cx| {
                        let container = MessageContainer::with_role(MessageRole::Assistant, cx);
                        container.set_current_request_id(request_id);
                        container.set_node_id(Some(node_id));
                        container.set_session_id(sid);
                        container
                    });
                    queue.push(assistant_container);
                }

                // Sync ListState if we pushed a new container
                self.notify_messages_appended(old_len, cx);
            }
            UiEvent::StreamingStopped {
                id,
                cancelled,
                error: _,
            } => {
                if cancelled {
                    self.update_all_messages(cx, |message_container, cx| {
                        message_container.remove_blocks_with_request_id(id, cx);
                    });
                    self.remove_empty_containers(cx);
                } else {
                    // Finish any open thinking blocks when streaming ends normally.
                    // This handles the edge case where the LLM response ends after
                    // thinking without producing text or tool calls.
                    self.update_last_message(cx, |message, cx| {
                        message.finish_any_thinking_blocks(cx);
                    });
                }
            }
            UiEvent::RollbackStreaming { id } => {
                // Discard all blocks produced by the failed request so the retry
                // starts with a clean slate (same mechanism as cancellation).
                self.update_all_messages(cx, |message_container, cx| {
                    message_container.remove_blocks_with_request_id(id, cx);
                });
                self.remove_empty_containers(cx);
            }
            UiEvent::RefreshChatList => {
                debug!("UI: RefreshChatList event received");
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    debug!("UI: Sending ListSessions to backend");
                    let _ = sender.try_send(BackendEvent::ListSessions);
                } else {
                    warn!("UI: No backend event sender available for RefreshChatList");
                }
            }
            UiEvent::UpdateChatList { sessions } => {
                debug!(
                    "UI: UpdateChatList event received with {} sessions",
                    sessions.len()
                );
                // Update local cache
                *self.chat_sessions.lock().unwrap() = sessions.clone();
                let _current_session_id = self.current_session_id.lock().unwrap().clone();

                // Refresh all windows to trigger re-render with new chat data
                debug!("UI: Refreshing windows for chat list update");
                cx.refresh();
            }

            UiEvent::ClearMessages => {
                debug!("UI: ClearMessages event");
                {
                    let mut queue = self.message_queue.lock().unwrap();
                    queue.clear();
                }
                self.notify_messages_reset(cx);
            }

            UiEvent::SendUserMessage {
                message,
                session_id,
                attachments,
                branch_parent_id,
            } => {
                debug!(
                    "UI: SendUserMessage event for session {}: {} (with {} attachments, branch_parent: {:?})",
                    session_id,
                    message,
                    attachments.len(),
                    branch_parent_id
                );
                // Clear any existing error when user sends a new message
                *self.current_error.lock().unwrap() = None;

                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::SendUserMessage {
                        session_id,
                        message,
                        attachments,
                        branch_parent_id,
                    });
                } else {
                    warn!("UI: No backend event sender available");
                }
            }
            UiEvent::UpdateSessionMetadata { metadata } => {
                debug!(
                    "UI: UpdateSessionMetadata event for session {}",
                    metadata.id
                );
                // Update the specific session in our local cache
                {
                    let mut sessions = self.chat_sessions.lock().unwrap();
                    if let Some(existing_session) =
                        sessions.iter_mut().find(|s| s.id == metadata.id)
                    {
                        *existing_session = metadata.clone();
                        debug!("Updated existing session metadata for {}", metadata.id);
                    } else {
                        // Session not found in cache, add it (shouldn't normally happen)
                        sessions.push(metadata.clone());
                        debug!("Added new session metadata for {}", metadata.id);
                    }
                }

                // If this is the current session, update the current project for parameter filtering

                if let Some(current_session_id) = self.current_session_id.lock().unwrap().as_ref() {
                    if *current_session_id == metadata.id {
                        // Store last_usage for the current session in a stable location
                        // (not in chat_sessions, which can be overwritten by stale disk data)
                        *self.current_session_last_usage.lock().unwrap() =
                            Some(metadata.last_usage.clone());

                        // Update MessagesView with current project
                        self.update_messages_view(cx, |messages_view, _cx| {
                            messages_view.set_current_project(metadata.initial_project.clone());
                        });

                        // Update all MessageContainers with current project
                        self.update_all_messages(cx, |container, _cx| {
                            container.set_current_project(metadata.initial_project.clone());
                        });
                    }
                }

                // Update the project sidebar entity specifically
                let persisted = self.persisted_projects.lock().unwrap().clone();
                self.update_project_sidebar(cx, |sidebar, cx| {
                    sidebar.set_persisted_projects(persisted);
                    // Get updated sessions list
                    let updated_sessions = self.chat_sessions.lock().unwrap().clone();
                    sidebar.update_sessions(updated_sessions, cx);
                    cx.notify();
                });
                debug!("UI: Updated project sidebar for session metadata change");
            }
            UiEvent::UpdateSessionActivityState {
                session_id,
                activity_state,
            } => {
                debug!(
                    "UI: UpdateSessionActivityState for session {} → {:?}",
                    session_id, activity_state
                );

                // Update the project sidebar
                self.update_project_sidebar(cx, |sidebar, cx| {
                    sidebar.update_single_session_activity_state(
                        session_id.clone(),
                        activity_state.clone(),
                        cx,
                    );
                });

                // Update current session activity state for messages view
                if let Some(current_session_id) = self.current_session_id.lock().unwrap().as_ref() {
                    if current_session_id == &session_id {
                        *self.current_session_activity_state.lock().unwrap() =
                            Some(activity_state.clone());

                        // Show/clear error banner based on session error state.
                        // This ensures the banner appears immediately when the
                        // currently viewed session enters the Errored state, and
                        // clears when it transitions away (e.g. new agent starts).
                        if let crate::session::instance::SessionActivityState::Errored { message } =
                            &activity_state
                        {
                            *self.current_error.lock().unwrap() = Some(message.clone());
                        } else {
                            // Clear any session error when state moves away from Errored
                            // (but only if the current error came from this session —
                            // we check by seeing if there's an error at all; backend
                            // errors are also stored here but those are transient and
                            // would have been cleared by now).
                            *self.current_error.lock().unwrap() = None;
                        }

                        cx.refresh();
                    }
                }
            }
            UiEvent::QueueUserMessage {
                message,
                session_id,
                attachments,
            } => {
                debug!(
                    "UI: QueueUserMessage event for session {}: {} (with {} attachments)",
                    session_id,
                    message,
                    attachments.len()
                );
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::QueueUserMessage {
                        session_id,
                        message,
                        attachments,
                    });
                }
            }
            UiEvent::RequestPendingMessageEdit { session_id } => {
                debug!(
                    "UI: RequestPendingMessageEdit event for session {}",
                    session_id
                );
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::RequestPendingMessageEdit { session_id });
                }
            }
            UiEvent::UpdatePendingMessage { message } => {
                debug!("UI: UpdatePendingMessage event with message: {:?}", message);
                // Update MessagesView's pending message
                self.update_messages_view(cx, |messages_view, cx| {
                    messages_view.update_pending_message(message.clone());
                    cx.notify();
                });
                // Refresh UI to trigger re-render
                cx.refresh();
            }
            UiEvent::AddImage { media_type, data } => {
                // Add image to the last message container
                self.update_last_message(cx, |message, cx| {
                    message.add_image_block(media_type, data, cx);
                });
            }

            UiEvent::AppendToolOutput { tool_id, chunk } => {
                // Append tool output to the last message container
                self.update_last_message(cx, |message, cx| {
                    message.append_tool_output(tool_id, chunk, cx);
                });
                // Terminal card height grows as new output lines arrive;
                // keep the chat scrolled to the bottom when following.
                self.auto_scroll_if_following(cx);
            }

            UiEvent::DisplayError { message } => {
                debug!("UI: DisplayError event with message: {}", message);
                // Store the error message in state
                *self.current_error.lock().unwrap() = Some(message);
                // Refresh UI to show the error popover
                cx.refresh();
            }
            UiEvent::ClearError => {
                debug!("UI: ClearError event");
                // Clear the error message from state
                *self.current_error.lock().unwrap() = None;

                // If the current session is in Errored state, tell the backend
                // to reset it to Idle so the sidebar icon disappears and the
                // error doesn't reappear on next session switch.
                let is_session_errored = self
                    .current_session_activity_state
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|s| {
                        matches!(
                            s,
                            crate::session::instance::SessionActivityState::Errored { .. }
                        )
                    });
                if is_session_errored {
                    if let Some(session_id) = self.current_session_id.lock().unwrap().clone() {
                        if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                            let _ = sender.try_send(BackendEvent::ClearSessionError { session_id });
                        }
                    }
                }

                // Refresh UI to hide the error popover
                cx.refresh();
            }
            UiEvent::ShowTransientStatus { message } => {
                debug!("UI: ShowTransientStatus: {}", message);
                *self.transient_status.lock().unwrap() = Some(message);
                cx.refresh();

                // Schedule auto-dismiss after 3 seconds via a background thread
                // that sends ClearTransientStatus back through the event channel.
                let sender = self.event_sender.lock().unwrap().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    let _ = sender.try_send(UiEvent::ClearTransientStatus);
                });
            }
            UiEvent::ClearTransientStatus => {
                *self.transient_status.lock().unwrap() = None;
                cx.refresh();
            }
            UiEvent::StartReasoningSummaryItem => {
                self.update_last_message(cx, |message, cx| {
                    message.start_reasoning_summary_item(cx);
                });
            }
            UiEvent::AppendReasoningSummaryDelta { delta } => {
                self.update_last_message(cx, |message, cx| {
                    message.append_reasoning_summary_delta(delta, cx);
                });
            }
            UiEvent::CompleteReasoning => {
                self.update_last_message(cx, |message, cx| {
                    message.complete_reasoning(cx);
                });
            }
            UiEvent::UpdateCurrentModel { model_name } => {
                debug!("UI: UpdateCurrentModel event with model: {}", model_name);
                // Store the current model
                *self.current_model.lock().unwrap() = Some(model_name);
                // Refresh UI to update the model selector
                cx.refresh();
            }
            UiEvent::UpdateSandboxPolicy { policy } => {
                debug!("UI: UpdateSandboxPolicy event with policy: {:?}", policy);
                *self.current_sandbox_policy.lock().unwrap() = Some(policy.clone());
                cx.refresh();
            }
            UiEvent::UpdateWorktreeData {
                worktrees,
                current_worktree_path,
                is_git_repo,
            } => {
                debug!(
                    "UI: UpdateWorktreeData event — {} worktrees, current_path={:?}, is_git_repo={}",
                    worktrees.len(), current_worktree_path, is_git_repo
                );
                *self.current_worktree_data.lock().unwrap() = Some(WorktreeData {
                    worktrees,
                    current_worktree_path,
                    is_git_repo,
                });
                cx.refresh();
            }

            UiEvent::RefreshCurrentSession { session_id } => {
                // Another process modified the session file on disk.
                // Use incremental refresh which diffs the active path and only
                // appends new messages (no-op if we wrote the change ourselves).
                debug!("UI: RefreshCurrentSession for {session_id}");
                let current = self.current_session_id.lock().unwrap().clone();
                if current.as_deref() == Some(session_id.as_str()) {
                    if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                        let _ = sender.try_send(BackendEvent::RefreshSession { session_id });
                    }
                }
            }

            // Resource events - logged for now, can be extended for features like "follow mode"
            UiEvent::ResourceLoaded { project, path } => {
                trace!(
                    "UI: ResourceLoaded event - project: {}, path: {}",
                    project,
                    path.display()
                );
            }
            UiEvent::ResourceWritten { project, path } => {
                trace!(
                    "UI: ResourceWritten event - project: {}, path: {}",
                    project,
                    path.display()
                );
            }
            UiEvent::DirectoryListed { project, path } => {
                trace!(
                    "UI: DirectoryListed event - project: {}, path: {}",
                    project,
                    path.display()
                );
            }
            UiEvent::ResourceDeleted { project, path } => {
                trace!(
                    "UI: ResourceDeleted event - project: {}, path: {}",
                    project,
                    path.display()
                );
            }

            UiEvent::CancelSubAgent { tool_id } => {
                debug!("UI: CancelSubAgent event for tool_id: {}", tool_id);
                // Forward to backend with current session ID
                if let Some(session_id) = self.current_session_id.lock().unwrap().clone() {
                    if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                        let _ = sender.try_send(BackendEvent::CancelSubAgent {
                            session_id,
                            tool_id,
                        });
                    }
                } else {
                    warn!("UI: CancelSubAgent requested but no active session");
                }
            }

            // === Session Branching Events ===
            UiEvent::StartMessageEdit {
                session_id,
                node_id,
            } => {
                debug!(
                    "UI: StartMessageEdit event for session {} node {}",
                    session_id, node_id
                );
                // Forward to backend to get message content
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::StartMessageEdit {
                        session_id,
                        node_id,
                    });
                }
            }
            UiEvent::SwitchBranch {
                session_id,
                new_node_id,
            } => {
                debug!(
                    "UI: SwitchBranch event for session {} to node {}",
                    session_id, new_node_id
                );
                // Forward to backend to perform branch switch
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::SwitchBranch {
                        session_id,
                        new_node_id,
                    });
                }
            }

            UiEvent::MessageEditReady {
                content,
                attachments,
                branch_parent_id,
                messages,
                tool_results,
            } => {
                debug!(
                    "UI: MessageEditReady event - content len: {}, attachments: {}, parent: {:?}, {} messages",
                    content.len(),
                    attachments.len(),
                    branch_parent_id,
                    messages.len()
                );

                // Get current session_id without holding lock during SetMessages processing
                let session_id = self.current_session_id.lock().unwrap().clone();

                // Get current project for new containers
                let current_project = if let Some(ref session_id) = session_id {
                    let sessions = self.chat_sessions.lock().unwrap();
                    sessions
                        .iter()
                        .find(|s| s.id == *session_id)
                        .map(|s| s.initial_project.clone())
                        .unwrap_or_default()
                } else {
                    String::new()
                };

                // Clear existing messages and rebuild with truncated set
                // (Inline version of SetMessages logic to avoid recursive call)
                {
                    let mut queue = self.message_queue.lock().unwrap();
                    queue.clear();
                }

                // Update MessagesView with current project and session ID
                if let Some(ref session_id) = session_id {
                    let session_id_for_messages = session_id.clone();
                    self.update_messages_view(cx, |messages_view, _cx| {
                        messages_view.set_current_project(current_project.clone());
                        messages_view.set_current_session_id(Some(session_id_for_messages));
                    });
                }

                // Process message data
                for message_data in messages {
                    let current_container = {
                        let mut queue = self.message_queue.lock().unwrap();

                        // Always create a new container per message (each MessageData
                        // corresponds to one persisted node / LLM request).
                        let container =
                            cx.new(|cx| MessageContainer::with_role(message_data.role.clone(), cx));

                        let node_id = message_data.node_id;
                        let branch_info = message_data.branch_info.clone();
                        let sid = session_id.clone();
                        self.update_container(&container, cx, |container, _cx| {
                            container.set_current_project(current_project.clone());
                            container.set_node_id(node_id);
                            container.set_branch_info(branch_info);
                            container.set_session_id(sid);
                        });

                        queue.push(container.clone());
                        container
                    };

                    self.process_fragments_for_container(
                        &current_container,
                        message_data.fragments,
                        cx,
                    );
                }

                // Apply tool results
                for tool_result in tool_results {
                    let ui_images: Vec<(String, String)> = tool_result
                        .images
                        .iter()
                        .map(|img| (img.media_type.clone(), img.base64_data.clone()))
                        .collect();

                    self.update_all_messages(cx, |message_container, cx| {
                        message_container.update_tool_status(
                            &tool_result.tool_id,
                            tool_result.status,
                            tool_result.message.clone(),
                            tool_result.output.clone(),
                            tool_result.styled_output.clone(),
                            tool_result.duration_seconds,
                            ui_images.clone(),
                            cx,
                        );
                    });
                }

                // Ensure we end with an Assistant container for the edit response
                {
                    let mut queue = self.message_queue.lock().unwrap();
                    let needs_assistant_container = if let Some(last) = queue.last() {
                        cx.update_entity(last, |message, _cx| message.is_user_message())
                    } else {
                        true
                    };

                    if needs_assistant_container {
                        let assistant_container =
                            cx.new(|cx| MessageContainer::with_role(MessageRole::Assistant, cx));
                        let sid = session_id.clone();
                        self.update_container(&assistant_container, cx, |c, _cx| {
                            c.set_session_id(sid);
                        });
                        queue.push(assistant_container);
                    }
                }

                // Sync ListState with fully rebuilt queue
                self.notify_messages_reset(cx);

                // Store pending edit state for RootView to pick up on refresh
                self.set_pending_edit(PendingEdit {
                    content,
                    attachments,
                    branch_parent_id,
                });

                // Refresh UI to trigger RootView to process the pending edit
                cx.refresh();
            }
            UiEvent::BranchSwitched {
                session_id,
                messages,
                tool_results,
                plan,
            } => {
                debug!(
                    "UI: BranchSwitched event for session {} with {} messages",
                    session_id,
                    messages.len()
                );
                // TODO Phase 4: Update messages display with new branch content
                // For now, we can reuse the SetMessages logic
                self.process_ui_event_async(
                    UiEvent::SetMessages {
                        messages,
                        session_id: Some(session_id),
                        tool_results,
                    },
                    cx,
                );
                self.process_ui_event_async(UiEvent::UpdatePlan { plan }, cx);
            }

            UiEvent::UpdateBranchInfo {
                node_id,
                branch_info,
            } => {
                debug!(
                    "UI: UpdateBranchInfo for node {} with {} siblings",
                    node_id,
                    branch_info.sibling_ids.len()
                );

                // Find the message container with this node_id and update its branch_info
                let queue = self.message_queue.lock().unwrap();
                for container in queue.iter() {
                    let container_node_id = cx.update_entity(container, |c, _cx| c.node_id());

                    if container_node_id == Some(node_id) {
                        let branch_info_clone = branch_info.clone();
                        self.update_container(container, cx, |c, _cx| {
                            c.set_branch_info(Some(branch_info_clone));
                        });
                        break;
                    }
                }

                cx.refresh();
            }

            UiEvent::ConfigChanged => {
                debug!("UI: ConfigChanged event — config files modified on disk");
                self.config_generation
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                // If no model is selected yet, try to resolve one from the
                // updated config (e.g. after onboarding sets a default).
                let current = self.current_model.lock().unwrap().clone();
                if current.is_none() || current.as_deref() == Some("") {
                    let settings = crate::ui::gpui::settings::UiSettings::load();
                    if let Some(ref default_model) = settings.default_model {
                        // Verify the model actually exists in config
                        if let Ok(config) = llm::provider_config::ConfigurationSystem::load() {
                            if config.get_model(default_model).is_some() {
                                *self.current_model.lock().unwrap() = Some(default_model.clone());
                                // Tell the backend to switch the active session's model
                                // and update the default for future sessions
                                if let Some(sender) =
                                    self.backend_event_sender.lock().unwrap().as_ref()
                                {
                                    let _ = sender.try_send(BackendEvent::UpdateDefaultModel {
                                        model_name: default_model.clone(),
                                    });
                                    if let Some(session_id) =
                                        self.current_session_id.lock().unwrap().clone()
                                    {
                                        let _ = sender.try_send(BackendEvent::SwitchModel {
                                            session_id,
                                            model_name: default_model.clone(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }

                cx.refresh();
            }

            UiEvent::PersistUiState => {
                // Cancel any pending save task and start a new one with a debounce
                // delay.  When the timer fires, dirty entries are taken from the
                // store and written to disk on a background thread.
                let task = cx.spawn(async move |cx: &mut gpui::AsyncApp| {
                    cx.background_executor()
                        .timer(ui_state::debounce_duration())
                        .await;
                    let files = if let Ok(mut store) = ui_state::UiStateStore::global().lock() {
                        store.take_dirty()
                    } else {
                        Vec::new()
                    };
                    if !files.is_empty() {
                        cx.background_spawn(async move {
                            ui_state::write_ui_state_files(files);
                        })
                        .await;
                    }
                });
                *self.ui_state_save_task.lock().unwrap() = Some(task);
            }
        }
    }

    /// Process display fragments and add them to a message container
    fn process_fragments_for_container(
        &self,
        container: &Entity<MessageContainer>,
        fragments: Vec<DisplayFragment>,
        cx: &mut gpui::AsyncApp,
    ) {
        for fragment in fragments {
            match fragment {
                DisplayFragment::PlainText(text) => {
                    self.update_container(container, cx, |container, cx| {
                        container.add_or_append_to_text_block(text, cx);
                    });
                }

                DisplayFragment::ThinkingText {
                    text,
                    duration_seconds,
                } => {
                    self.update_container(container, cx, |container, cx| {
                        container.add_or_append_to_thinking_block_with_duration(
                            text,
                            duration_seconds,
                            cx,
                        );
                    });
                }

                DisplayFragment::ToolName {
                    name,
                    id,
                    duration_seconds,
                } => {
                    self.update_container(container, cx, |container, cx| {
                        container.add_tool_use_block_with_duration(name, id, duration_seconds, cx);
                    });
                }
                DisplayFragment::ToolParameter {
                    name,
                    value,
                    tool_id,
                } => {
                    self.update_container(container, cx, |container, cx| {
                        container.add_or_update_tool_parameter(tool_id, name, value, cx);
                    });
                }
                DisplayFragment::ToolEnd { id } => {
                    self.update_container(container, cx, |container, cx| {
                        container.end_tool_use(id, cx);
                    });
                }
                DisplayFragment::Image { media_type, data } => {
                    self.update_container(container, cx, |container, cx| {
                        container.add_image_block(media_type, data, cx);
                    });
                }
                DisplayFragment::CompactionDivider { summary } => {
                    self.update_container(container, cx, |container, cx| {
                        container.add_compaction_divider(summary.clone(), cx);
                    });
                }
                DisplayFragment::ReasoningSummaryStart => {
                    self.update_container(container, cx, |container, cx| {
                        container.start_reasoning_summary_item(cx);
                    });
                }
                DisplayFragment::ReasoningSummaryDelta(delta) => {
                    self.update_container(container, cx, |container, cx| {
                        container.append_reasoning_summary_delta(delta, cx);
                    });
                }
                DisplayFragment::ToolOutput { tool_id, chunk } => {
                    self.update_container(container, cx, |container, cx| {
                        container.append_tool_output(tool_id, chunk, cx);
                    });
                }

                DisplayFragment::ToolTerminal { .. } => {
                    // The GPUI terminal executor registers the tool→terminal
                    // mapping directly in the TerminalPool, so no action needed
                    // during fragment replay.
                }

                DisplayFragment::ReasoningComplete => {
                    self.update_container(container, cx, |container, cx| {
                        container.complete_reasoning(cx);
                    });
                }
                DisplayFragment::HiddenToolCompleted => {
                    self.update_container(container, cx, |container, cx| {
                        container.mark_hidden_tool_completed(cx);
                    });
                }
            }
        }
    }
}
