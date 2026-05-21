//! Backend response handling.
//!
//! Contains `Gpui::handle_backend_response` — processes responses from the
//! background session management thread (session created/loaded/deleted, etc.).

use crate::ui::backend::BackendResponse;
use gpui::AsyncApp;
use tracing::{debug, info, warn};

use super::super::*;

impl Gpui {
    pub(in crate::ui::gpui) fn handle_backend_response(
        &self,
        response: BackendResponse,
        cx: &mut AsyncApp,
    ) {
        match response {
            BackendResponse::SessionCreated { session_id } => {
                debug!("Received BackendResponse::SessionCreated");
                *self.current_session_id.lock().unwrap() = Some(session_id.clone());
                // Refresh the session list
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::ListSessions);
                    // Load the newly created session to connect it to the UI
                    let _ = sender.try_send(BackendEvent::LoadSession { session_id });
                }
            }

            BackendResponse::SessionDeleted { session_id } => {
                debug!("Received BackendResponse::SessionDeleted");
                // Clean up collapse-state overrides for the deleted session
                blocks::ToolCollapseState::remove_session(&session_id);

                // If the deleted session was the currently active one, disconnect
                // from it so the messages view shows the "no session" hint.
                // (This may already have been done eagerly in the delete-request
                // handler in root.rs, in which case the check is a no-op.)
                let is_current =
                    self.current_session_id.lock().unwrap().as_deref() == Some(session_id.as_str());
                if is_current {
                    debug!("Deleted session was the active session — clearing UI state");
                    self.clear_current_session_state();

                    // Tell MessagesView there is no session
                    self.update_messages_view(cx, |view, cx| {
                        view.set_current_session_id(None);
                        view.messages_reset(0, cx);
                        cx.notify();
                    });
                }

                // Refresh the session list
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::ListSessions);
                }
                cx.refresh();
            }
            BackendResponse::SessionsListed { sessions } => {
                debug!("Received BackendResponse::SessionsListed");
                *self.chat_sessions.lock().unwrap() = sessions.clone();
                self.push_event(UiEvent::UpdateChatList { sessions });
            }
            BackendResponse::Error { message } => {
                warn!("Backend error: {}", message);
                // Display the error to the user
                self.push_event(UiEvent::DisplayError { message });
            }
            BackendResponse::PendingMessageForEdit {
                session_id,
                message: _,
            } => {
                debug!(
                    "Received BackendResponse::PendingMessageForEdit for session {}",
                    session_id
                );
                // TODO: Move pending message to text input field for editing
                // For now, clear the pending message display
                self.push_event(UiEvent::UpdatePendingMessage { message: None });
            }
            BackendResponse::PendingMessageUpdated {
                session_id,
                message,
            } => {
                debug!(
                    "Received BackendResponse::PendingMessageUpdated for session {}",
                    session_id
                );
                // Only update pending message display if this is for the current session
                if let Some(current_session_id) = self.current_session_id.lock().unwrap().as_ref() {
                    if current_session_id == &session_id {
                        self.push_event(UiEvent::UpdatePendingMessage { message });
                    }
                }
            }
            BackendResponse::ModelSwitched {
                session_id,
                model_name,
            } => {
                let current_session_id = self.current_session_id.lock().unwrap().clone();
                if current_session_id.as_deref() == Some(session_id.as_str()) {
                    debug!(
                        "Received BackendResponse::ModelSwitched for active session {}: {}",
                        session_id, model_name
                    );
                    self.push_event(UiEvent::UpdateCurrentModel {
                        model_name: model_name.clone(),
                    });
                } else {
                    debug!(
                        "Ignoring BackendResponse::ModelSwitched for session {} (current: {:?})",
                        session_id, current_session_id
                    );
                }
            }

            BackendResponse::SandboxPolicyChanged { session_id, policy } => {
                let current_session_id = self.current_session_id.lock().unwrap().clone();
                if current_session_id.as_deref() == Some(session_id.as_str()) {
                    debug!(
                        "Received BackendResponse::SandboxPolicyChanged for active session {}",
                        session_id
                    );
                    self.push_event(UiEvent::UpdateSandboxPolicy { policy });
                } else {
                    debug!(
                        "Ignoring BackendResponse::SandboxPolicyChanged for session {} (current: {:?})",
                        session_id, current_session_id
                    );
                }
            }

            BackendResponse::SubAgentCancelled {
                session_id,
                tool_id,
            } => {
                debug!(
                    "Received BackendResponse::SubAgentCancelled for tool {} in session {}",
                    tool_id, session_id
                );
                // The sub-agent will update its own UI state via the normal tool output mechanism
                // No additional UI update needed here
            }

            // Session branching responses
            BackendResponse::MessageEditReady {
                session_id,
                content,
                attachments,
                branch_parent_id,
                messages,
                tool_results,
            } => {
                debug!(
                    "Received BackendResponse::MessageEditReady for session {} with {} chars, {} attachments, {} messages",
                    session_id,
                    content.len(),
                    attachments.len(),
                    messages.len()
                );

                // Forward to UI as event
                self.process_ui_event_async(
                    UiEvent::MessageEditReady {
                        content: content.clone(),
                        attachments: attachments.clone(),
                        branch_parent_id,
                        messages: messages.clone(),
                        tool_results: tool_results.clone(),
                    },
                    cx,
                );
            }

            BackendResponse::BranchSwitched {
                session_id,
                messages,
                tool_results,
                plan,
            } => {
                debug!(
                    "Received BackendResponse::BranchSwitched for session {} with {} messages",
                    session_id,
                    messages.len()
                );
                // Forward to UI as event
                self.process_ui_event_async(
                    UiEvent::BranchSwitched {
                        session_id: session_id.clone(),
                        messages: messages.clone(),
                        tool_results: tool_results.clone(),
                        plan: plan.clone(),
                    },
                    cx,
                );
            }
            BackendResponse::MessageEditCancelled {
                session_id,
                messages,
                tool_results,
            } => {
                debug!(
                    "Received BackendResponse::MessageEditCancelled for session {} with {} messages",
                    session_id,
                    messages.len()
                );

                // Forward to UI as event - reuse SetMessages to restore the view
                self.process_ui_event_async(
                    UiEvent::SetMessages {
                        messages: messages.clone(),
                        session_id: Some(session_id.clone()),
                        tool_results: tool_results.clone(),
                    },
                    cx,
                );
            }

            // Git worktree responses — forwarded to the WorktreeSelector component
            BackendResponse::BranchesAndWorktreesListed {
                session_id,
                worktrees,
                current_branch: _,
                is_git_repo,
                ..
            } => {
                let current_session_id = self.current_session_id.lock().unwrap().clone();
                if current_session_id.as_deref() == Some(session_id.as_str()) {
                    debug!(
                        "Received BranchesAndWorktreesListed for active session {}: {} worktrees, is_git_repo={}",
                        session_id, worktrees.len(), is_git_repo
                    );
                    // Preserve the current selection (set by WorktreeSwitched / WorktreeCreated)
                    let existing_path = self
                        .current_worktree_data
                        .lock()
                        .unwrap()
                        .as_ref()
                        .and_then(|d| d.current_worktree_path.clone());
                    self.push_event(UiEvent::UpdateWorktreeData {
                        worktrees,
                        current_worktree_path: existing_path,
                        is_git_repo,
                    });
                } else {
                    debug!(
                        "Ignoring BranchesAndWorktreesListed for session {} (current: {:?})",
                        session_id, current_session_id
                    );
                }
            }
            BackendResponse::WorktreeSwitched {
                session_id,
                worktree_path,
                branch,
            } => {
                let current_session_id = self.current_session_id.lock().unwrap().clone();
                if current_session_id.as_deref() == Some(session_id.as_str()) {
                    info!(
                        "Worktree switched for active session {}: path={:?}, branch={:?}",
                        session_id, worktree_path, branch
                    );
                    // Update the stored worktree data with the new selection, preserving the list
                    let current_data = self.current_worktree_data.lock().unwrap().clone();
                    let (worktrees, is_git_repo) = current_data
                        .map(|d| (d.worktrees, d.is_git_repo))
                        .unwrap_or_default();
                    self.push_event(UiEvent::UpdateWorktreeData {
                        worktrees,
                        current_worktree_path: worktree_path,
                        is_git_repo,
                    });
                    // Also refresh the full list since worktrees may have changed
                    if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                        let _ =
                            sender.try_send(BackendEvent::ListBranchesAndWorktrees { session_id });
                    }
                }
            }
            BackendResponse::WorktreeCreated {
                session_id,
                worktree_path,
                branch,
            } => {
                let current_session_id = self.current_session_id.lock().unwrap().clone();
                if current_session_id.as_deref() == Some(session_id.as_str()) {
                    info!(
                        "Worktree created for active session {}: {:?} (branch: {})",
                        session_id, worktree_path, branch
                    );
                    // Update selection to the newly created worktree, preserving the existing list
                    let current_data = self.current_worktree_data.lock().unwrap().clone();
                    let (worktrees, is_git_repo) = current_data
                        .map(|d| (d.worktrees, d.is_git_repo))
                        .unwrap_or_default();
                    self.push_event(UiEvent::UpdateWorktreeData {
                        worktrees,
                        current_worktree_path: Some(worktree_path),
                        is_git_repo,
                    });
                    // Refresh the full list to include the new worktree
                    if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                        let _ =
                            sender.try_send(BackendEvent::ListBranchesAndWorktrees { session_id });
                    }
                }
            }
            BackendResponse::ProjectAdded {
                project_name,
                session_id,
            } => {
                info!(
                    "Project '{}' added, initial session: {}",
                    project_name, session_id
                );
                *self.current_session_id.lock().unwrap() = Some(session_id.clone());
                // Refresh the session list and load the new session
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::ListSessions);
                    let _ = sender.try_send(BackendEvent::LoadSession { session_id });
                }
            }
            BackendResponse::ProjectPersisted { project_name } => {
                info!("Project '{}' persisted to projects.json", project_name);
                // Update the set of persisted projects so the sidebar can
                // remove the "pin" icon for this project.
                self.persisted_projects.lock().unwrap().insert(project_name);
                // Trigger a re-render so the sidebar picks up the change.
                cx.refresh();
            }
            BackendResponse::ProjectAlreadyExists { project_name } => {
                info!("Project '{}' already exists — nothing to do", project_name);
            }
        }
    }
}
