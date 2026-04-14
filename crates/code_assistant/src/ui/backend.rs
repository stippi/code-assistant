use crate::config::{save_project, DefaultProjectManager};
use crate::persistence::{ChatMetadata, DraftAttachment, SessionModelConfig};
use crate::session::SessionManager;
use crate::types::Project;
use crate::ui::gpui::terminal_executor::GpuiTerminalCommandExecutor;
use crate::ui::UserInterface;
use crate::utils::content::content_blocks_from;
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
        initial_project: Option<String>,
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
        /// If set, creates a new branch from this parent node instead of appending to active path
        branch_parent_id: Option<crate::persistence::NodeId>,
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
    CancelMessageEdit {
        session_id: String,
    },

    // Git worktree management
    ListBranchesAndWorktrees {
        session_id: String,
    },
    SwitchWorktree {
        session_id: String,
        worktree_path: Option<PathBuf>,
        branch: Option<String>,
    },

    #[allow(dead_code)]
    CreateWorktree {
        session_id: String,
        branch_name: String,
        base_branch: Option<String>,
    },

    /// Add a new project to projects.json and create an initial session
    AddProject {
        name: String,
        path: PathBuf,
    },

    /// Clear the Errored state on a session (user dismissed the error banner)
    ClearSessionError {
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
        /// Messages up to (but not including) the message being edited
        messages: Vec<crate::ui::ui_events::MessageData>,
        tool_results: Vec<crate::ui::ui_events::ToolResultData>,
    },

    BranchSwitched {
        session_id: String,
        messages: Vec<crate::ui::ui_events::MessageData>,
        tool_results: Vec<crate::ui::ui_events::ToolResultData>,
        plan: crate::types::PlanState,
    },

    MessageEditCancelled {
        session_id: String,
        messages: Vec<crate::ui::ui_events::MessageData>,
        tool_results: Vec<crate::ui::ui_events::ToolResultData>,
    },

    // Git worktree responses
    BranchesAndWorktreesListed {
        session_id: String,
        #[allow(dead_code)]
        branches: Vec<git::Branch>,
        worktrees: Vec<git::Worktree>,
        #[allow(dead_code)]
        current_branch: Option<String>,
        is_git_repo: bool,
    },
    WorktreeSwitched {
        session_id: String,
        worktree_path: Option<PathBuf>,
        branch: Option<String>,
    },

    WorktreeCreated {
        session_id: String,
        worktree_path: PathBuf,
        branch: String,
    },

    /// A new project was added and an initial session was created for it
    ProjectAdded {
        project_name: String,
        session_id: String,
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

            BackendEvent::CreateNewSession {
                name,
                initial_project,
            } => Some(handle_create_session(&multi_session_manager, name, initial_project).await),

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
                branch_parent_id,
            } => {
                handle_send_user_message(
                    &multi_session_manager,
                    &session_id,
                    &message,
                    &attachments,
                    branch_parent_id,
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

            BackendEvent::CancelMessageEdit { session_id } => {
                Some(handle_cancel_message_edit(&multi_session_manager, &session_id).await)
            }

            BackendEvent::ListBranchesAndWorktrees { session_id } => {
                Some(handle_list_branches_and_worktrees(&multi_session_manager, &session_id).await)
            }

            BackendEvent::SwitchWorktree {
                session_id,
                worktree_path,
                branch,
            } => Some(
                handle_switch_worktree(&multi_session_manager, &session_id, worktree_path, branch)
                    .await,
            ),

            BackendEvent::CreateWorktree {
                session_id,
                branch_name,
                base_branch,
            } => Some(
                handle_create_worktree(
                    &multi_session_manager,
                    &session_id,
                    &branch_name,
                    base_branch.as_deref(),
                )
                .await,
            ),

            BackendEvent::AddProject { name, path } => {
                Some(handle_add_project(&multi_session_manager, &name, &path).await)
            }

            BackendEvent::ClearSessionError { session_id } => {
                let mut manager = multi_session_manager.lock().await;
                if let Some(session) = manager.get_session_mut(&session_id) {
                    let current = session.get_activity_state();
                    if current.is_terminal() {
                        session.set_activity_state(
                            crate::session::instance::SessionActivityState::Idle,
                        );
                    }
                }
                // Broadcast the state change so the sidebar updates
                let _ = ui
                    .send_event(crate::ui::UiEvent::UpdateSessionActivityState {
                        session_id,
                        activity_state: crate::session::instance::SessionActivityState::Idle,
                    })
                    .await;
                None // No backend response needed
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
    initial_project: Option<String>,
) -> BackendResponse {
    let create_result = {
        let mut manager = multi_session_manager.lock().await;
        if let Some(project) = initial_project {
            // Create a session config override with the specified project
            let mut config = manager.session_config_template().clone();
            config.initial_project = project;
            manager.create_session_with_config(name.clone(), Some(config), None)
        } else {
            manager.create_session(name.clone())
        }
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
    branch_parent_id: Option<crate::persistence::NodeId>,
    runtime_options: &BackendRuntimeOptions,
    ui: &Arc<dyn UserInterface>,
) -> Option<BackendResponse> {
    debug!(
        "User message for session {}: {} (with {} attachments, branch_parent: {:?})",
        session_id,
        message,
        attachments.len(),
        branch_parent_id
    );

    // Convert DraftAttachments to ContentBlocks
    let content_blocks = content_blocks_from(message, attachments);

    // First, add the user message to the session and get the new node_id
    let (new_node_id, branch_info_updates) = {
        let mut manager = multi_session_manager.lock().await;
        match manager.add_user_message(session_id, content_blocks.clone(), branch_parent_id) {
            Ok(node_id) => {
                // If we created a branch, get branch info updates for all siblings
                let updates = if branch_parent_id.is_some() {
                    manager.get_sibling_branch_infos(session_id, node_id)
                } else {
                    Vec::new()
                };
                (Some(node_id), updates)
            }
            Err(e) => {
                error!("Failed to add user message to session: {}", e);
                return Some(BackendResponse::Error {
                    message: format!("Failed to add user message: {e}"),
                });
            }
        }
    };

    // Now display the user message with the correct node_id
    if let Err(e) = ui
        .send_event(crate::ui::UiEvent::DisplayUserInput {
            content: message.to_string(),
            attachments: attachments.to_vec(),
            node_id: new_node_id,
        })
        .await
    {
        error!("Failed to display user message with attachments: {}", e);
    }

    // Send branch info updates for all siblings (so they show the branch switcher)
    for (sibling_node_id, branch_info) in branch_info_updates {
        if let Err(e) = ui
            .send_event(crate::ui::UiEvent::UpdateBranchInfo {
                node_id: sibling_node_id,
                branch_info,
            })
            .await
        {
            error!("Failed to send branch info update: {}", e);
        }
    }

    // Start the agent (message already added)
    let result = {
        let project_manager = Box::new(DefaultProjectManager::new());
        let command_executor = Box::new(GpuiTerminalCommandExecutor::new(session_id.to_string()));
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
                    // Message already added via add_user_message above
                    manager
                        .start_agent_for_session(
                            session_id,
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
                                width: None,
                                height: None,
                            }),
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                };

                // The branch parent is the parent of the node being edited
                let branch_parent_id = node.parent_id;

                // Generate truncated messages (up to but not including the message being edited)
                let messages = session_instance
                    .convert_messages_to_ui_data_until(
                        session_instance.session.config.tool_syntax,
                        branch_parent_id,
                    )
                    .unwrap_or_default();

                let tool_results = session_instance
                    .convert_tool_executions_to_ui_data()
                    .unwrap_or_default();

                Ok((
                    content,
                    attachments,
                    branch_parent_id,
                    messages,
                    tool_results,
                ))
            } else {
                Err(anyhow::anyhow!("Message node {} not found", node_id))
            }
        } else {
            Err(anyhow::anyhow!("Session {} not found", session_id))
        }
    };

    match result {
        Ok((content, attachments, branch_parent_id, messages, tool_results)) => {
            BackendResponse::MessageEditReady {
                session_id: session_id.to_string(),
                content,
                attachments,
                branch_parent_id,
                messages,
                tool_results,
            }
        }
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

    // Persist the updated active_path
    if let Err(e) = manager.save_session(session_id) {
        error!("Failed to save session after branch switch: {}", e);
        // Continue anyway - the switch worked in memory
    }

    // Re-get session reference after save (borrow checker)
    let Some(session_instance) = manager.get_session(session_id) else {
        return BackendResponse::Error {
            message: format!("Session {} not found after save", session_id),
        };
    };

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

async fn handle_cancel_message_edit(
    multi_session_manager: &Arc<Mutex<SessionManager>>,
    session_id: &str,
) -> BackendResponse {
    debug!("Cancelling message edit for session {}", session_id);

    let manager = multi_session_manager.lock().await;

    let Some(session_instance) = manager.get_session(session_id) else {
        return BackendResponse::Error {
            message: format!("Session {} not found", session_id),
        };
    };

    // Reload the current messages (restore full active path)
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

    BackendResponse::MessageEditCancelled {
        session_id: session_id.to_string(),
        messages: messages_data,
        tool_results,
    }
}

// ============================================================================
// Git Worktree Handlers
// ============================================================================

/// Resolve the project root path for a session (init_path, not worktree_path).
#[allow(clippy::result_large_err)]
fn get_session_project_root(
    manager: &SessionManager,
    session_id: &str,
) -> Result<PathBuf, BackendResponse> {
    let session = manager
        .get_session(session_id)
        .ok_or_else(|| BackendResponse::Error {
            message: format!("Session {session_id} not found"),
        })?;

    session
        .session
        .config
        .init_path
        .clone()
        .ok_or_else(|| BackendResponse::Error {
            message: "Session has no project path configured".to_string(),
        })
}

async fn handle_list_branches_and_worktrees(
    multi_session_manager: &Arc<Mutex<SessionManager>>,
    session_id: &str,
) -> BackendResponse {
    debug!("Listing branches and worktrees for session {}", session_id);

    let project_root = {
        let manager = multi_session_manager.lock().await;
        match get_session_project_root(&manager, session_id) {
            Ok(path) => path,
            Err(resp) => return resp,
        }
    };

    // Check if project is a git repo
    if !git::GitRepository::is_repo(&project_root) {
        return BackendResponse::BranchesAndWorktreesListed {
            session_id: session_id.to_string(),
            branches: Vec::new(),
            worktrees: Vec::new(),
            current_branch: None,
            is_git_repo: false,
        };
    }

    let repo = match git::GitRepository::open(&project_root) {
        Ok(repo) => repo,
        Err(e) => {
            error!("Failed to open git repository: {}", e);
            return BackendResponse::Error {
                message: format!("Failed to open git repository: {e}"),
            };
        }
    };

    let branches = match repo.list_branches() {
        Ok(b) => b,
        Err(e) => {
            error!("Failed to list branches: {}", e);
            return BackendResponse::Error {
                message: format!("Failed to list branches: {e}"),
            };
        }
    };

    let current_branch = repo.current_branch();

    let worktrees = match git::worktree::list_worktrees(&repo.git, repo.workdir()).await {
        Ok(w) => w,
        Err(e) => {
            error!("Failed to list worktrees: {}", e);
            // Non-fatal: return branches without worktree info
            Vec::new()
        }
    };

    BackendResponse::BranchesAndWorktreesListed {
        session_id: session_id.to_string(),
        branches,
        worktrees,
        current_branch,
        is_git_repo: true,
    }
}

async fn handle_switch_worktree(
    multi_session_manager: &Arc<Mutex<SessionManager>>,
    session_id: &str,
    worktree_path: Option<PathBuf>,
    branch: Option<String>,
) -> BackendResponse {
    debug!(
        "Switching worktree for session {}: path={:?}, branch={:?}",
        session_id, worktree_path, branch
    );

    let result = {
        let mut manager = multi_session_manager.lock().await;
        manager.set_session_worktree(session_id, worktree_path.clone(), branch.clone())
    };

    match result {
        Ok(()) => {
            info!(
                "Successfully switched worktree for session {}: {:?}",
                session_id, worktree_path
            );
            BackendResponse::WorktreeSwitched {
                session_id: session_id.to_string(),
                worktree_path,
                branch,
            }
        }
        Err(e) => {
            error!(
                "Failed to switch worktree for session {}: {}",
                session_id, e
            );
            BackendResponse::Error {
                message: format!("Failed to switch worktree: {e}"),
            }
        }
    }
}

async fn handle_create_worktree(
    multi_session_manager: &Arc<Mutex<SessionManager>>,
    session_id: &str,
    branch_name: &str,
    base_branch: Option<&str>,
) -> BackendResponse {
    debug!(
        "Creating worktree for session {}: branch={}, base={:?}",
        session_id, branch_name, base_branch
    );

    let project_root = {
        let manager = multi_session_manager.lock().await;
        match get_session_project_root(&manager, session_id) {
            Ok(path) => path,
            Err(resp) => return resp,
        }
    };

    let repo = match git::GitRepository::open(&project_root) {
        Ok(repo) => repo,
        Err(e) => {
            return BackendResponse::Error {
                message: format!("Failed to open git repository: {e}"),
            };
        }
    };

    // Check if a worktree for this branch already exists
    match git::worktree::find_worktree_for_branch(&repo.git, repo.workdir(), branch_name).await {
        Ok(Some(existing)) => {
            info!(
                "Reusing existing worktree for branch '{}' at {:?}",
                branch_name, existing.path
            );
            // Reuse existing worktree — just switch the session to it
            let result = {
                let mut manager = multi_session_manager.lock().await;
                manager.set_session_worktree(
                    session_id,
                    Some(existing.path.clone()),
                    Some(branch_name.to_string()),
                )
            };
            return match result {
                Ok(()) => BackendResponse::WorktreeCreated {
                    session_id: session_id.to_string(),
                    worktree_path: existing.path,
                    branch: branch_name.to_string(),
                },
                Err(e) => BackendResponse::Error {
                    message: format!("Failed to set worktree on session: {e}"),
                },
            };
        }
        Ok(None) => {} // No existing worktree, create one
        Err(e) => {
            debug!("Could not check existing worktrees: {}", e);
            // Continue with creation attempt
        }
    }

    let worktree_path = git::worktree::suggest_worktree_path(repo.workdir(), branch_name);

    match git::worktree::create_worktree(
        &repo.git,
        repo.workdir(),
        &worktree_path,
        branch_name,
        base_branch,
    )
    .await
    {
        Ok(canonical_path) => {
            info!(
                "Created worktree at {:?} for branch '{}'",
                canonical_path, branch_name
            );

            // Update the session to use this worktree
            let result = {
                let mut manager = multi_session_manager.lock().await;
                manager.set_session_worktree(
                    session_id,
                    Some(canonical_path.clone()),
                    Some(branch_name.to_string()),
                )
            };

            match result {
                Ok(()) => BackendResponse::WorktreeCreated {
                    session_id: session_id.to_string(),
                    worktree_path: canonical_path,
                    branch: branch_name.to_string(),
                },
                Err(e) => BackendResponse::Error {
                    message: format!("Worktree created but failed to update session: {e}"),
                },
            }
        }
        Err(e) => {
            error!("Failed to create worktree: {}", e);
            BackendResponse::Error {
                message: format!("Failed to create worktree: {e}"),
            }
        }
    }
}

// ============================================================================
// Project Management Handlers
// ============================================================================

async fn handle_add_project(
    multi_session_manager: &Arc<Mutex<SessionManager>>,
    name: &str,
    path: &PathBuf,
) -> BackendResponse {
    info!("Adding project '{}' at {:?}", name, path);

    // Save to projects.json
    let project = Project {
        path: path.clone(),
        format_on_save: None,
    };
    if let Err(e) = save_project(name, &project) {
        error!("Failed to save project to config: {}", e);
        return BackendResponse::Error {
            message: format!("Failed to save project: {e}"),
        };
    }

    // Create an initial session for the new project
    let create_result = {
        let mut manager = multi_session_manager.lock().await;
        let mut config = manager.session_config_template().clone();
        config.initial_project = name.to_string();
        manager.create_session_with_config(None, Some(config), None)
    };

    match create_result {
        Ok(session_id) => {
            info!(
                "Created initial session {} for project '{}'",
                session_id, name
            );
            BackendResponse::ProjectAdded {
                project_name: name.to_string(),
                session_id,
            }
        }
        Err(e) => {
            // Project was saved but session creation failed
            error!("Project saved but failed to create session: {}", e);
            BackendResponse::Error {
                message: format!("Project saved but failed to create session: {e}"),
            }
        }
    }
}
