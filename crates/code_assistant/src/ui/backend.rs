use crate::config::DefaultProjectManager;
use crate::persistence::{ChatMetadata, DraftAttachment};
use crate::ui::UserInterface;
use crate::utils::{content::content_blocks_from, DefaultCommandExecutor};
use llm::factory::{create_llm_client, LLMClientConfig};
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
}

pub async fn handle_backend_events(
    backend_event_rx: async_channel::Receiver<BackendEvent>,
    backend_response_tx: async_channel::Sender<BackendResponse>,
    multi_session_manager: Arc<Mutex<crate::session::SessionManager>>,
    cfg: Arc<LLMClientConfig>,
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
                    &cfg,
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
    multi_session_manager: &Arc<Mutex<crate::session::SessionManager>>,
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
    multi_session_manager: &Arc<Mutex<crate::session::SessionManager>>,
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
    multi_session_manager: &Arc<Mutex<crate::session::SessionManager>>,
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
    multi_session_manager: &Arc<Mutex<crate::session::SessionManager>>,
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
    multi_session_manager: &Arc<Mutex<crate::session::SessionManager>>,
    session_id: &str,
    message: &str,
    attachments: &[DraftAttachment],
    cfg: &Arc<LLMClientConfig>,
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

        // Check if session has stored LLM config, otherwise use global config
        let llm_config = {
            let manager = multi_session_manager.lock().await;
            manager.get_session_llm_config(session_id).unwrap_or(None)
        };

        let effective_config = llm_config
            .map(|session_config| {
                llm::factory::LLMClientConfig {
                    provider: session_config.provider,
                    model: session_config.model,
                    base_url: session_config.base_url,
                    aicore_config: session_config.aicore_config,
                    num_ctx: session_config.num_ctx,
                    record_path: session_config.record_path,
                    playback_path: None,  // Always None for session config
                    fast_playback: false, // Always false for session config
                }
            })
            .unwrap_or_else(|| cfg.as_ref().clone());

        let llm_client = create_llm_client(effective_config).await;

        match llm_client {
            Ok(client) => {
                let mut manager = multi_session_manager.lock().await;
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
    multi_session_manager: &Arc<Mutex<crate::session::SessionManager>>,
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
    multi_session_manager: &Arc<Mutex<crate::session::SessionManager>>,
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
