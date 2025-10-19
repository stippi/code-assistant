use crate::config::DefaultProjectManager;
use crate::persistence::{ChatMetadata, DraftAttachment, SessionModelConfig};
use crate::session::SessionManager;
use crate::ui::UserInterface;
use crate::utils::{content::content_blocks_from, DefaultCommandExecutor};
use llm::factory::create_llm_client_from_model;

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
}

pub async fn handle_backend_events(
    backend_event_rx: async_channel::Receiver<BackendEvent>,
    backend_response_tx: async_channel::Sender<BackendResponse>,
    multi_session_manager: Arc<Mutex<SessionManager>>,
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
                session_config.record_path.clone(),
                false,
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

    // Create new session model config
    let new_model_config = SessionModelConfig {
        model_name: model_name.to_string(),
        record_path: None, // Keep existing recording path if any
    };

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
