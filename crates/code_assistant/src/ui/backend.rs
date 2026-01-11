use crate::config::DefaultProjectManager;
use crate::persistence::{ChatMetadata, DraftAttachment, SessionModelConfig};
use crate::session::SessionManager;
use crate::ui::UserInterface;
use crate::utils::content::content_blocks_from;
use command_executor::DefaultCommandExecutor;
use llm::factory::create_llm_client_from_model;
use llm::provider_config::ConfigurationSystem;
use sandbox::SandboxPolicy;

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, trace};

// Unified event type for all UI→Backend communication
#[derive(Debug, Clone)]
pub enum BackendEvent {
    // Session management
    LoadSession {
        session_id: String,
    },
    CreateNewSession {
        name: Option<String>,
    },
    DeleteSession {
        session_id: String,
    },
    ListSessions,

    // Agent operations
    SendUserMessage {
        session_id: String,
        message: String,
        attachments: Vec<DraftAttachment>,
    },
    QueueUserMessage {
        session_id: String,
        message: String,
        attachments: Vec<DraftAttachment>,
    },
    RequestPendingMessageEdit {
        session_id: String,
    },

    // Model management
    SwitchModel {
        session_id: String,
        model_name: String,
    },
    ChangeSandboxPolicy {
        session_id: String,
        policy: SandboxPolicy,
    },

    // Sub-agent management
    CancelSubAgent {
        session_id: String,
        tool_id: String,
    },

    // Session branching
    StartMessageEdit {
        session_id: String,
        node_id: crate::persistence::NodeId,
    },
    SwitchBranch {
        session_id: String,
        new_node_id: crate::persistence::NodeId,
    },
}

// Response from backend to UI
#[derive(Debug, Clone)]
pub enum BackendResponse {
    SessionCreated {
        session_id: String,
    },
    #[allow(dead_code)]
    SessionDeleted {
        session_id: String,
    },
    SessionsListed {
        sessions: Vec<ChatMetadata>,
    },
    Error {
        message: String,
    },
    PendingMessageForEdit {
        session_id: String,
        #[allow(dead_code)]
        message: String,
    },
    PendingMessageUpdated {
        session_id: String,
        message: Option<String>,
    },
    ModelSwitched {
        session_id: String,
        model_name: String,
    },

    SandboxPolicyChanged {
        session_id: String,
        policy: SandboxPolicy,
    },

    SubAgentCancelled {
        session_id: String,
        tool_id: String,
    },

    // Session branching responses
    MessageEditReady {
        session_id: String,
        content: String,
        attachments: Vec<DraftAttachment>,
        branch_parent_id: Option<crate::persistence::NodeId>,
    },
    BranchSwitched {
        session_id: String,
        messages: Vec<crate::ui::ui_events::MessageData>,
        tool_results: Vec<crate::ui::ui_events::ToolResultData>,
        plan: crate::types::PlanState,
    },
}

#[derive(Debug, Clone)]
pub struct BackendRuntimeOptions {
    pub record_path: Option<PathBuf>,
    pub playback_path: Option<PathBuf>,
    pub fast_playback: bool,
}

pub async fn handle_backend_events(
    backend_event_rx: async_channel::Receiver<BackendEvent>,
    backend_response_tx: async_channel::Sender<BackendResponse>,
    multi_session_manager: Arc<Mutex<SessionManager>>,
    runtime_options: Arc<BackendRuntimeOptions>,
    ui: Arc<dyn UserInterface>,
) {
    debug!("Backend event handler started");

    while let Ok(event) = backend_event_rx.recv().await {
        debug!("Backend event: {:?}", event);

        let response = match event {
            BackendEvent::ListSessions => Some(handle_list_sessions(&multi_session_manager).await),

            BackendEvent::CreateNewSession { name } => {
                Some(handle_create_session(&multi_session_manager, name).await)
            }

            BackendEvent::LoadSession { session_id } => {
                handle_load_session(&multi_session_manager, &session_id, &ui).await
            }

            BackendEvent::DeleteSession { session_id } => {
                Some(handle_delete_session(&multi_session_manager, &session_id).await)
            }

            BackendEvent::SendUserMessage {
                session_id,
                message,
                attachments,
            } => {
                handle_send_user_message(
                    &multi_session_manager,
                    &session_id,
                    &message,
                    &attachments,
                    runtime_options.as_ref(),
                    &ui,
                )
                .await
            }

            BackendEvent::QueueUserMessage {
                session_id,
                message,
                attachments,
            } => Some(
                handle_queue_user_message(
                    &multi_session_manager,
                    &session_id,
                    &message,
                    &attachments,
                )
                .await,
            ),

            BackendEvent::RequestPendingMessageEdit { session_id } => {
                Some(handle_request_pending_message_edit(&multi_session_manager, &session_id).await)
            }

            BackendEvent::SwitchModel {
                session_id,
                model_name,
            } => Some(handle_switch_model(&multi_session_manager, &session_id, &model_name).await),
            BackendEvent::ChangeSandboxPolicy { session_id, policy } => Some(
                handle_change_sandbox_policy(&multi_session_manager, &session_id, policy).await,
            ),

            BackendEvent::CancelSubAgent {
                session_id,
                tool_id,
            } => Some(handle_cancel_sub_agent(&multi_session_manager, &session_id, &tool_id).await),

            BackendEvent::StartMessageEdit {
                session_id,
                node_id,
            } => {
                Some(handle_start_message_edit(&multi_session_manager, &session_id, node_id).await)
            }

            BackendEvent::SwitchBranch {
                session_id,
                new_node_id,
            } => Some(handle_switch_branch(&multi_session_manager, &session_id, new_node_id).await),
        };

        // Send response back to UI only if there is one
        if let Some(response) = response {
            if let Err(e) = backend_response_tx.send(response).await {
                error!("Failed to send response: {}", e);
                break;
            }
        }
    }

    debug!("Backend event handler stopped");
}

async fn handle_list_sessions(
    multi_session_manager: &Arc<Mutex<SessionManager>>,
) -> BackendResponse {
    let sessions = {
        let manager = multi_session_manager.lock().await;
        manager.list_all_sessions()
    };
    match sessions {
        Ok(sessions) => {
            trace!("Found {} sessions", sessions.len());
            BackendResponse::SessionsListed { sessions }
        }
        Err(e) => {
            error!("Failed to list sessions: {}", e);
            BackendResponse::Error {
                message: e.to_string(),
            }
        }
    }
}

async fn handle_create_session(
    multi_session_manager: &Arc<Mutex<SessionManager>>,
    name: Option<String>,
) -> BackendResponse {
    let create_result = {
        let mut manager = multi_session_manager.lock().await;
        manager.create_session(name.clone())
    };

    match create_result {
        Ok(session_id) => {
            info!("Created session {}", session_id);
            BackendResponse::SessionCreated { session_id }
        }
        Err(e) => {
            error!("Failed to create session: {}", e);
            BackendResponse::Error {
                message: e.to_string(),
            }
        }
    }
}

async fn handle_load_session(
    multi_session_manager: &Arc<Mutex<SessionManager>>,
    session_id: &str,
    ui: &Arc<dyn UserInterface>,
) -> Option<BackendResponse> {
    debug!("LoadSession requested: {}", session_id);

    let ui_events_result = {
        let mut manager = multi_session_manager.lock().await;
        manager.set_active_session(session_id.to_string()).await
    };

    match ui_events_result {
        Ok(ui_events) => {
            trace!("Session connected with {} UI events", ui_events.len());

            // Send all UI events to update the interface
            for event in ui_events {
                if let Err(e) = ui.send_event(event).await {
                    error!("Failed to send UI event: {}", e);
                }
            }
            // No response needed - UI events already handled the update
            None
        }
        Err(e) => {
            error!("Failed to connect to session {}: {}", session_id, e);
            Some(BackendResponse::Error {
                message: e.to_string(),
            })
        }
    }
}

async fn handle_delete_session(
    multi_session_manager: &Arc<Mutex<SessionManager>>,
    session_id: &str,
) -> BackendResponse {
    debug!("DeleteSession requested: {}", session_id);

    let delete_result = {
        let mut manager = multi_session_manager.lock().await;
        manager.delete_session(session_id)
    };

    match delete_result {
        Ok(_) => {
            debug!("Session deleted: {}", session_id);
            BackendResponse::SessionDeleted {
                session_id: session_id.to_string(),
            }
        }
        Err(e) => {
            error!("Failed to delete session {}: {}", session_id, e);
            BackendResponse::Error {
                message: e.to_string(),
            }
        }
    }
}

async fn handle_send_user_message(
    multi_session_manager: &Arc<Mutex<SessionManager>>,
    session_id: &str,
    message: &str,
    attachments: &[DraftAttachment],
    runtime_options: &BackendRuntimeOptions,
    ui: &Arc<dyn UserInterface>,
) -> Option<BackendResponse> {
    debug!(
        "User message for session {}: {} (with {} attachments)",
        session_id,
        message,
        attachments.len()
    );

    // Convert DraftAttachments to ContentBlocks
    let content_blocks = content_blocks_from(message, attachments);

    // Display the user message with attachments in the UI
    if let Err(e) = ui
        .send_event(crate::ui::UiEvent::DisplayUserInput {
            content: message.to_string(),
            attachments: attachments.to_vec(),
        })
        .await
    {
        error!("Failed to display user message with attachments: {}", e);
    }

    // Start the agent with structured content
    let result = {
        let project_manager = Box::new(DefaultProjectManager::new());
        let command_executor = Box::new(DefaultCommandExecutor);
        let user_interface = ui.clone();

        // Check if session has stored model config, otherwise use global config
        let session_config = {
            let manager = multi_session_manager.lock().await;
            manager.get_session_model_config(session_id).unwrap_or(None)
        };

        // Use model-based configuration system
        let llm_client = if let Some(ref session_config) = session_config {
            // Use session's stored model
            create_llm_client_from_model(
                &session_config.model_name,
                runtime_options.playback_path.clone(),
                runtime_options.fast_playback,
                runtime_options.record_path.clone(),
            )
            .await
        } else {
            // No session config - this should not happen in the new system
            return Some(BackendResponse::Error {
                message: "Session has no model configuration. Please ensure all sessions are created with a model.".to_string(),
            });
        };

        match llm_client {
            Ok(client) => {
                let mut manager = multi_session_manager.lock().await;
                if let Err(e) = manager.set_session_model_config(session_id, session_config.clone())
                {
                    error!(
                        "Failed to persist model config for session {}: {}",
                        session_id, e
                    );
                    Err(e)
                } else {
                    manager
                        .start_agent_for_message(
                            session_id,
                            content_blocks,
                            client,
                            project_manager,
                            command_executor,
                            user_interface,
                            None,
                        )
                        .await
                }
            }
            Err(e) => {
                error!("Failed to create LLM client: {}", e);
                Err(e)
            }
        }
    };

    match result {
        Ok(_) => {
            debug!("Agent started for session {}", session_id);
            // No response needed - agent is running
            None
        }
        Err(e) => {
            error!("Failed to start agent for session {}: {}", session_id, e);
            Some(BackendResponse::Error {
                message: format!("Failed to start agent: {e}"),
            })
        }
    }
}

async fn handle_queue_user_message(
    multi_session_manager: &Arc<Mutex<SessionManager>>,
    session_id: &str,
    message: &str,
    attachments: &[DraftAttachment],
) -> BackendResponse {
    debug!(
        "Queue user message with attachments for session {}: {} (with {} attachments)",
        session_id,
        message,
        attachments.len()
    );

    // Convert DraftAttachments to ContentBlocks
    let content_blocks = content_blocks_from(message, attachments);

    let result = {
        let mut manager = multi_session_manager.lock().await;
        manager.queue_structured_user_message(session_id, content_blocks)
    };

    match result {
        Ok(_) => {
            debug!("Message with attachments queued for session {}", session_id);
            let pending_message = {
                let manager = multi_session_manager.lock().await;
                manager.get_pending_message(session_id).unwrap_or(None)
            };
            BackendResponse::PendingMessageUpdated {
                session_id: session_id.to_string(),
                message: pending_message,
            }
        }
        Err(e) => {
            error!(
                "Failed to queue message with attachments for session {}: {}",
                session_id, e
            );
            BackendResponse::Error {
                message: format!("Failed to queue message: {e}"),
            }
        }
    }
}

async fn handle_request_pending_message_edit(
    multi_session_manager: &Arc<Mutex<SessionManager>>,
    session_id: &str,
) -> BackendResponse {
    debug!("Request pending message edit for session {}", session_id);

    let result = {
        let mut manager = multi_session_manager.lock().await;
        manager.request_pending_message_for_edit(session_id)
    };

    match result {
        Ok(Some(message)) => {
            debug!("Retrieved pending message for editing: {}", message);
            BackendResponse::PendingMessageForEdit {
                session_id: session_id.to_string(),
                message,
            }
        }
        Ok(None) => {
            debug!("No pending message found for session {}", session_id);
            BackendResponse::PendingMessageUpdated {
                session_id: session_id.to_string(),
                message: None,
            }
        }
        Err(e) => {
            error!(
                "Failed to get pending message for session {}: {}",
                session_id, e
            );
            BackendResponse::Error {
                message: format!("Failed to get pending message: {e}"),
            }
        }
    }
}

async fn handle_switch_model(
    multi_session_manager: &Arc<Mutex<SessionManager>>,
    session_id: &str,
    model_name: &str,
) -> BackendResponse {
    debug!(
        "Switching model for session {} to {}",
        session_id, model_name
    );

    // Validate the requested model exists
    let config_system = match ConfigurationSystem::load() {
        Ok(system) => system,
        Err(e) => {
            error!("Failed to load model configuration: {}", e);
            return BackendResponse::Error {
                message: format!("Failed to load model configuration: {e}"),
            };
        }
    };

    if config_system.get_model(model_name).is_none() {
        error!("Model '{}' not found in configuration", model_name);
        return BackendResponse::Error {
            message: format!("Model '{model_name}' not found in configuration."),
        };
    }

    let new_model_config = SessionModelConfig::new(model_name.to_string());

    let result = {
        let mut manager = multi_session_manager.lock().await;
        manager.set_session_model_config(session_id, Some(new_model_config))
    };

    match result {
        Ok(()) => {
            info!(
                "Successfully switched model for session {} to {}",
                session_id, model_name
            );
            BackendResponse::ModelSwitched {
                session_id: session_id.to_string(),
                model_name: model_name.to_string(),
            }
        }
        Err(e) => {
            error!("Failed to switch model for session {}: {}", session_id, e);
            BackendResponse::Error {
                message: format!("Failed to switch model: {e}"),
            }
        }
    }
}

async fn handle_change_sandbox_policy(
    multi_session_manager: &Arc<Mutex<SessionManager>>,
    session_id: &str,
    policy: SandboxPolicy,
) -> BackendResponse {
    let result = {
        let mut manager = multi_session_manager.lock().await;
        manager.set_session_sandbox_policy(session_id, policy.clone())
    };

    match result {
        Ok(()) => BackendResponse::SandboxPolicyChanged {
            session_id: session_id.to_string(),
            policy,
        },
        Err(e) => BackendResponse::Error {
            message: format!("Failed to update sandbox policy: {e}"),
        },
    }
}

async fn handle_cancel_sub_agent(
    multi_session_manager: &Arc<Mutex<SessionManager>>,
    session_id: &str,
    tool_id: &str,
) -> BackendResponse {
    debug!(
        "Cancelling sub-agent {} for session {}",
        tool_id, session_id
    );

    let result = {
        let manager = multi_session_manager.lock().await;
        manager.cancel_sub_agent(session_id, tool_id)
    };

    match result {
        Ok(true) => {
            info!(
                "Successfully cancelled sub-agent {} for session {}",
                tool_id, session_id
            );
            BackendResponse::SubAgentCancelled {
                session_id: session_id.to_string(),
                tool_id: tool_id.to_string(),
            }
        }
        Ok(false) => {
            debug!(
                "Sub-agent {} not found or already completed for session {}",
                tool_id, session_id
            );
            // Not really an error - the sub-agent may have already completed
            BackendResponse::SubAgentCancelled {
                session_id: session_id.to_string(),
                tool_id: tool_id.to_string(),
            }
        }

        Err(e) => {
            error!(
                "Failed to cancel sub-agent {} for session {}: {}",
                tool_id, session_id, e
            );
            BackendResponse::Error {
                message: format!("Failed to cancel sub-agent: {e}"),
            }
        }
    }
}

// ============================================================================
// Session Branching Handlers
// ============================================================================

async fn handle_start_message_edit(
    multi_session_manager: &Arc<Mutex<SessionManager>>,
    session_id: &str,
    node_id: crate::persistence::NodeId,
) -> BackendResponse {
    debug!(
        "Starting message edit for session {} node {}",
        session_id, node_id
    );

    let result = {
        let manager = multi_session_manager.lock().await;
        if let Some(session_instance) = manager.get_session(session_id) {
            // Get the message node
            if let Some(node) = session_instance.session.message_nodes.get(&node_id) {
                // Extract content from message
                let content = match &node.message.content {
                    llm::MessageContent::Text(text) => text.clone(),
                    llm::MessageContent::Structured(blocks) => {
                        // Extract text content from structured blocks
                        blocks
                            .iter()
                            .filter_map(|block| match block {
                                llm::ContentBlock::Text { text, .. } => Some(text.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                };

                // Extract attachments (images) from message
                let attachments = match &node.message.content {
                    llm::MessageContent::Structured(blocks) => blocks
                        .iter()
                        .filter_map(|block| match block {
                            llm::ContentBlock::Image {
                                media_type, data, ..
                            } => Some(DraftAttachment::Image {
                                content: data.clone(),
                                mime_type: media_type.clone(),
                            }),
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                };

                // The branch parent is the parent of the node being edited
                let branch_parent_id = node.parent_id;

                Ok((content, attachments, branch_parent_id))
            } else {
                Err(anyhow::anyhow!("Message node {} not found", node_id))
            }
        } else {
            Err(anyhow::anyhow!("Session {} not found", session_id))
        }
    };

    match result {
        Ok((content, attachments, branch_parent_id)) => BackendResponse::MessageEditReady {
            session_id: session_id.to_string(),
            content,
            attachments,
            branch_parent_id,
        },
        Err(e) => {
            error!("Failed to start message edit: {}", e);
            BackendResponse::Error {
                message: format!("Failed to start message edit: {e}"),
            }
        }
    }
}

async fn handle_switch_branch(
    multi_session_manager: &Arc<Mutex<SessionManager>>,
    session_id: &str,
    new_node_id: crate::persistence::NodeId,
) -> BackendResponse {
    debug!(
        "Switching branch for session {} to node {}",
        session_id, new_node_id
    );

    let mut manager = multi_session_manager.lock().await;

    let Some(session_instance) = manager.get_session_mut(session_id) else {
        return BackendResponse::Error {
            message: format!("Session {} not found", session_id),
        };
    };

    // Perform the branch switch
    if let Err(e) = session_instance.session.switch_branch(new_node_id) {
        error!("Failed to switch branch: {}", e);
        return BackendResponse::Error {
            message: format!("Failed to switch branch: {e}"),
        };
    }

    // Generate new messages for UI
    let messages_data = match session_instance
        .convert_messages_to_ui_data(session_instance.session.config.tool_syntax)
    {
        Ok(data) => data,
        Err(e) => {
            error!("Failed to convert messages: {}", e);
            return BackendResponse::Error {
                message: format!("Failed to convert messages: {e}"),
            };
        }
    };

    let tool_results = match session_instance.convert_tool_executions_to_ui_data() {
        Ok(results) => results,
        Err(e) => {
            error!("Failed to convert tool results: {}", e);
            return BackendResponse::Error {
                message: format!("Failed to convert tool results: {e}"),
            };
        }
    };

    let plan = session_instance.session.plan.clone();

    BackendResponse::BranchSwitched {
        session_id: session_id.to_string(),
        messages: messages_data,
        tool_results,
        plan,
    }
}
