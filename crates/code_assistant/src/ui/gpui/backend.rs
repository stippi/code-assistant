use crate::config::DefaultProjectManager;
use crate::persistence::DraftAttachment;
use crate::ui::{gpui::Gpui, UserInterface};
use crate::utils::{content::content_blocks_from, DefaultCommandExecutor};
use llm::factory::{LLMClientConfig, create_llm_client};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, trace};

pub async fn handle_backend_events(
    backend_event_rx: async_channel::Receiver<crate::ui::gpui::BackendEvent>,
    backend_response_tx: async_channel::Sender<crate::ui::gpui::BackendResponse>,
    multi_session_manager: Arc<Mutex<crate::session::SessionManager>>,
    cfg: Arc<LLMClientConfig>,
    gui: Gpui,
) {
    debug!("Backend event handler started");

    while let Ok(event) = backend_event_rx.recv().await {
        debug!("Backend event: {:?}", event);

        let response = match event {
            crate::ui::gpui::BackendEvent::ListSessions => {
                handle_list_sessions(&multi_session_manager).await
            }

            crate::ui::gpui::BackendEvent::CreateNewSession { name } => {
                handle_create_session(&multi_session_manager, name).await
            }

            crate::ui::gpui::BackendEvent::LoadSession { session_id } => {
                handle_load_session(&multi_session_manager, &session_id, &gui).await
            }

            crate::ui::gpui::BackendEvent::DeleteSession { session_id } => {
                handle_delete_session(&multi_session_manager, &session_id).await
            }

            crate::ui::gpui::BackendEvent::SendUserMessage {
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
                    &gui,
                ).await
            }

            crate::ui::gpui::BackendEvent::QueueUserMessage {
                session_id,
                message,
                attachments,
            } => {
                handle_queue_user_message(
                    &multi_session_manager,
                    &session_id,
                    &message,
                    &attachments,
                ).await
            }

            crate::ui::gpui::BackendEvent::RequestPendingMessageEdit { session_id } => {
                handle_request_pending_message_edit(&multi_session_manager, &session_id).await
            }
        };

        // Send response back to UI
        if let Err(e) = backend_response_tx.send(response).await {
            error!("Failed to send response: {}", e);
            break;
        }
    }

    debug!("Backend event handler stopped");
}

async fn handle_list_sessions(
    multi_session_manager: &Arc<Mutex<crate::session::SessionManager>>,
) -> crate::ui::gpui::BackendResponse {
    let sessions = {
        let manager = multi_session_manager.lock().await;
        manager.list_all_sessions()
    };
    match sessions {
        Ok(sessions) => {
            trace!("Found {} sessions", sessions.len());
            crate::ui::gpui::BackendResponse::SessionsListed { sessions }
        }
        Err(e) => {
            error!("Failed to list sessions: {}", e);
            crate::ui::gpui::BackendResponse::Error {
                message: e.to_string(),
            }
        }
    }
}

async fn handle_create_session(
    multi_session_manager: &Arc<Mutex<crate::session::SessionManager>>,
    name: Option<String>,
) -> crate::ui::gpui::BackendResponse {
    let create_result = {
        let mut manager = multi_session_manager.lock().await;
        manager.create_session(name.clone())
    };

    match create_result {
        Ok(session_id) => {
            info!("Created session {}", session_id);
            crate::ui::gpui::BackendResponse::SessionCreated { session_id }
        }
        Err(e) => {
            error!("Failed to create session: {}", e);
            crate::ui::gpui::BackendResponse::Error {
                message: e.to_string(),
            }
        }
    }
}

async fn handle_load_session(
    multi_session_manager: &Arc<Mutex<crate::session::SessionManager>>,
    session_id: &str,
    gui: &Gpui,
) -> crate::ui::gpui::BackendResponse {
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
                if let Err(e) = gui.send_event(event).await {
                    error!("Failed to send UI event: {}", e);
                }
            }

            // Don't return a response - UI events already handled the update
            return crate::ui::gpui::BackendResponse::SessionsListed { sessions: vec![] }; // Dummy response
        }
        Err(e) => {
            error!("Failed to connect to session {}: {}", session_id, e);
            crate::ui::gpui::BackendResponse::Error {
                message: e.to_string(),
            }
        }
    }
}

async fn handle_delete_session(
    multi_session_manager: &Arc<Mutex<crate::session::SessionManager>>,
    session_id: &str,
) -> crate::ui::gpui::BackendResponse {
    debug!("DeleteSession requested: {}", session_id);

    let delete_result = {
        let mut manager = multi_session_manager.lock().await;
        manager.delete_session(session_id)
    };

    match delete_result {
        Ok(_) => {
            debug!("Session deleted: {}", session_id);
            crate::ui::gpui::BackendResponse::SessionDeleted {
                session_id: session_id.to_string(),
            }
        }
        Err(e) => {
            error!("Failed to delete session {}: {}", session_id, e);
            crate::ui::gpui::BackendResponse::Error {
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
    gui: &Gpui,
) -> crate::ui::gpui::BackendResponse {
    debug!(
        "User message for session {}: {} (with {} attachments)",
        session_id,
        message,
        attachments.len()
    );

    // Convert DraftAttachments to ContentBlocks
    let content_blocks = content_blocks_from(message, attachments);

    // Display the user message with attachments in the UI
    if let Err(e) = gui
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
        let user_interface: Arc<dyn UserInterface> = Arc::new(gui.clone());

        // Check if session has stored LLM config, otherwise use global config
        let llm_config = {
            let manager = multi_session_manager.lock().await;
            manager.get_session_llm_config(session_id).unwrap_or(None)
        };

        let effective_config = llm_config.map(|session_config| {
            llm::factory::LLMClientConfig {
                provider: session_config.provider,
                model: session_config.model,
                base_url: session_config.base_url,
                aicore_config: session_config.aicore_config,
                num_ctx: session_config.num_ctx,
                record_path: session_config.record_path,
                playback_path: None, // Always None for session config
                fast_playback: false, // Always false for session config
            }
        }).unwrap_or_else(|| cfg.as_ref().clone());

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
            // Continue without returning a response since agent is running
            return crate::ui::gpui::BackendResponse::SessionsListed { sessions: vec![] }; // Dummy response
        }
        Err(e) => {
            error!("Failed to start agent for session {}: {}", session_id, e);
            crate::ui::gpui::BackendResponse::Error {
                message: format!("Failed to start agent: {e}"),
            }
        }
    }
}

async fn handle_queue_user_message(
    multi_session_manager: &Arc<Mutex<crate::session::SessionManager>>,
    session_id: &str,
    message: &str,
    attachments: &[DraftAttachment],
) -> crate::ui::gpui::BackendResponse {
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
            crate::ui::gpui::BackendResponse::PendingMessageUpdated {
                session_id: session_id.to_string(),
                message: pending_message,
            }
        }
        Err(e) => {
            error!(
                "Failed to queue message with attachments for session {}: {}",
                session_id, e
            );
            crate::ui::gpui::BackendResponse::Error {
                message: format!("Failed to queue message: {e}"),
            }
        }
    }
}

async fn handle_request_pending_message_edit(
    multi_session_manager: &Arc<Mutex<crate::session::SessionManager>>,
    session_id: &str,
) -> crate::ui::gpui::BackendResponse {
    debug!("Request pending message edit for session {}", session_id);

    let result = {
        let mut manager = multi_session_manager.lock().await;
        manager.request_pending_message_for_edit(session_id)
    };

    match result {
        Ok(Some(message)) => {
            debug!("Retrieved pending message for editing: {}", message);
            crate::ui::gpui::BackendResponse::PendingMessageForEdit {
                session_id: session_id.to_string(),
                message,
            }
        }
        Ok(None) => {
            debug!("No pending message found for session {}", session_id);
            crate::ui::gpui::BackendResponse::PendingMessageUpdated {
                session_id: session_id.to_string(),
                message: None,
            }
        }
        Err(e) => {
            error!(
                "Failed to get pending message for session {}: {}",
                session_id, e
            );
            crate::ui::gpui::BackendResponse::Error {
                message: format!("Failed to get pending message: {e}"),
            }
        }
    }
}
