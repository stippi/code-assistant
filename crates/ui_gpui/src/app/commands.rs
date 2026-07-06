//! UI→core commands.
//!
//! Every user action that asks the core to *do* something goes through the
//! typed [`SessionService`] methods here. Each command runs as a detached
//! task on the GPUI background executor; its typed result (or error) is
//! applied to the shared state and pushed into the UI event queue, which is
//! the single ingestion point for display updates on the foreground thread.

use code_assistant_core::persistence::{DraftAttachment, NodeId};
use code_assistant_core::session::service::{AddProjectOutcome, SessionService};
use std::future::Future;
use std::path::PathBuf;
use tracing::{debug, info, warn};

use super::super::*;

impl Gpui {
    /// Install the session service handle. Called by the wiring before
    /// `run_app`.
    pub fn set_session_service(&self, service: SessionService) {
        *self.session_service.lock().unwrap() = Some(service);
    }

    pub(crate) fn session_service(&self) -> Option<SessionService> {
        let service = self.session_service.lock().unwrap().clone();
        if service.is_none() {
            warn!("Session service not available");
        }
        service
    }

    /// Run a command future as a detached task on the GPUI background
    /// executor. Commands only touch thread-safe state and the UI event
    /// queue, never entities.
    pub(crate) fn dispatch(&self, fut: impl Future<Output = ()> + Send + 'static) {
        let executor = self.background_executor.lock().unwrap().clone();
        if let Some(executor) = executor {
            executor.spawn(fut).detach();
        } else {
            warn!("Cannot dispatch session command: app not running yet");
        }
    }

    fn display_error(&self, message: String) {
        self.push_event(UiEvent::DisplayError { message });
    }

    fn is_current_session(&self, session_id: &str) -> bool {
        self.current_session_id.lock().unwrap().as_deref() == Some(session_id)
    }

    // ========================================================================
    // Sessions
    // ========================================================================

    /// Fetch the session list and publish it to the sidebar.
    pub(crate) fn cmd_refresh_chat_list(&self) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            match service.list_sessions().await {
                Ok(sessions) => {
                    *gpui.chat_sessions.lock().unwrap() = sessions.clone();
                    gpui.push_event(UiEvent::UpdateChatList { sessions });
                }
                Err(e) => gpui.display_error(format!("Failed to list sessions: {e:#}")),
            }
        });
    }

    /// Create a session and connect it to the UI.
    pub(crate) fn cmd_create_session(&self, name: Option<String>, initial_project: Option<String>) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            match service.create_session(name, initial_project).await {
                Ok(session_id) => {
                    *gpui.current_session_id.lock().unwrap() = Some(session_id.clone());
                    gpui.cmd_refresh_chat_list();
                    gpui.cmd_load_session(session_id, None);
                }
                Err(e) => gpui.display_error(format!("Failed to create session: {e:#}")),
            }
        });
    }

    /// Connect a session to the UI: apply the returned snapshot and refresh
    /// the skill catalog for the `/skill` picker.
    pub(crate) fn cmd_load_session(&self, session_id: String, edit_until_node_id: Option<NodeId>) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            match service
                .load_session(session_id.clone(), edit_until_node_id)
                .await
            {
                Ok(snapshot) => {
                    gpui.apply_snapshot(&snapshot);
                    gpui.refresh_skills(session_id);
                }
                Err(e) => gpui.display_error(format!("Failed to load session: {e:#}")),
            }
        });
    }

    /// Ask the running agent of a session to stop. The local stop-request
    /// set drives the cancel button state; the actual stop happens core-side
    /// at the agent's next streaming checkpoint.
    pub(crate) fn cmd_request_stop(&self, session_id: String) {
        self.session_stop_requests
            .lock()
            .unwrap()
            .insert(session_id.clone());
        let Some(service) = self.session_service() else {
            return;
        };
        self.dispatch(async move {
            if let Err(e) = service.request_stop(session_id).await {
                debug!("Failed to request agent stop: {e:#}");
            }
        });
    }

    /// Delete a session. The caller is responsible for disconnecting the
    /// messages view first if the session is currently shown (see the
    /// sidebar delete handler in `main_screen`).
    pub(crate) fn cmd_delete_session(&self, session_id: String) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            match service.delete_session(session_id.clone()).await {
                Ok(()) => {
                    // Clean up collapse-state overrides for the deleted session
                    blocks::ToolCollapseState::remove_session(&session_id);
                    if gpui.is_current_session(&session_id) {
                        gpui.clear_current_session_state();
                    }
                    gpui.cmd_refresh_chat_list();
                }
                Err(e) => gpui.display_error(format!("Failed to delete session: {e:#}")),
            }
        });
    }

    /// Incremental refresh after another process changed the session file.
    pub(crate) fn cmd_refresh_session(&self, session_id: String) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            if let Err(e) = service.refresh_session(session_id).await {
                debug!("Session refresh failed: {e:#}");
                let _ = gpui;
            }
        });
    }

    /// Reset an Errored session back to Idle (user dismissed the banner).
    pub(crate) fn cmd_clear_session_error(&self, session_id: String) {
        let Some(service) = self.session_service() else {
            return;
        };
        self.dispatch(async move {
            if let Err(e) = service.clear_session_error(session_id).await {
                debug!("Failed to clear session error: {e:#}");
            }
        });
    }

    // ========================================================================
    // Agent
    // ========================================================================

    pub(crate) fn cmd_send_user_message(
        &self,
        session_id: String,
        message: String,
        attachments: Vec<DraftAttachment>,
        branch_parent_id: Option<NodeId>,
    ) {
        let Some(service) = self.session_service() else {
            return;
        };
        // Clear any existing error when the user sends a new message
        *self.current_error.lock().unwrap() = None;
        let gpui = self.clone();
        self.dispatch(async move {
            if let Err(e) = service
                .send_user_message(session_id, message, attachments, branch_parent_id)
                .await
            {
                gpui.display_error(format!("Failed to send message: {e:#}"));
            }
        });
    }

    pub(crate) fn cmd_queue_user_message(
        &self,
        session_id: String,
        message: String,
        attachments: Vec<DraftAttachment>,
    ) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            match service
                .queue_user_message(session_id.clone(), message, attachments)
                .await
            {
                Ok(pending) => {
                    if gpui.is_current_session(&session_id) {
                        gpui.push_event(UiEvent::UpdatePendingMessage { message: pending });
                    }
                }
                Err(e) => gpui.display_error(format!("Failed to queue message: {e:#}")),
            }
        });
    }

    /// Resume an errored/killed session against its existing history.
    pub(crate) fn cmd_resume_session(&self, session_id: String) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            if let Err(e) = service.resume_session(session_id).await {
                gpui.display_error(format!("{e:#}"));
            }
        });
    }

    /// Cancel a running sub-agent of the current session by tool id.
    pub(crate) fn cmd_cancel_sub_agent(&self, tool_id: String) {
        let Some(session_id) = self.current_session_id.lock().unwrap().clone() else {
            warn!("CancelSubAgent requested but no active session");
            return;
        };
        let Some(service) = self.session_service() else {
            return;
        };
        self.dispatch(async move {
            match service.cancel_sub_agent(session_id, tool_id).await {
                // The sub-agent updates its own tool card via the normal
                // tool-output mechanism; nothing else to apply here.
                Ok(_cancelled) => {}
                Err(e) => debug!("Failed to cancel sub-agent: {e:#}"),
            }
        });
    }

    // ========================================================================
    // Skills
    // ========================================================================

    /// Refresh the cached skill catalog used by the `/skill` input-area
    /// completion.
    pub fn refresh_skills(&self, session_id: String) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            match service.list_skills(session_id).await {
                Ok(skills) => gpui.set_skills(skills),
                Err(e) => debug!("Failed to list skills: {e:#}"),
            }
        });
    }

    pub(crate) fn cmd_invoke_skill(&self, session_id: String, scope: String, name: String) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            if let Err(e) = service.invoke_skill(session_id, scope, name).await {
                gpui.display_error(format!("{e:#}"));
            }
        });
    }

    // ========================================================================
    // Model & sandbox
    // ========================================================================

    pub(crate) fn cmd_switch_model(&self, session_id: String, model_name: String) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            // Model/allowed-models updates arrive via the broadcast stream;
            // only the caller shows the interaction-scoped warning.
            match service.switch_model(session_id.clone(), model_name).await {
                Ok(result) => {
                    if let Some(message) = result.warning {
                        if gpui.is_current_session(&session_id) {
                            gpui.push_event(UiEvent::ShowTransientStatus { message });
                        }
                    }
                }
                Err(e) => gpui.display_error(format!("{e:#}")),
            }
        });
    }

    pub(crate) fn cmd_update_default_model(&self, model_name: String) {
        let Some(service) = self.session_service() else {
            return;
        };
        self.dispatch(async move {
            if let Err(e) = service.update_default_model(model_name).await {
                debug!("Failed to update default model: {e:#}");
            }
        });
    }

    pub(crate) fn cmd_change_sandbox_policy(
        &self,
        session_id: String,
        policy: sandbox::SandboxPolicy,
    ) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            // The UpdateSandboxPolicy notification arrives via the stream.
            if let Err(e) = service.change_sandbox_policy(session_id, policy).await {
                gpui.display_error(format!("{e:#}"));
            }
        });
    }

    pub(crate) fn cmd_change_permission_tier(
        &self,
        session_id: String,
        tier: tools_core::PermissionTier,
    ) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            // The UpdatePermissionTier notification arrives via the stream.
            if let Err(e) = service.change_permission_tier(session_id, tier).await {
                gpui.display_error(format!("{e:#}"));
            }
        });
    }

    /// Answer a pending tool permission request. The prompt dismisses when
    /// the ToolPermissionRequestResolved notification arrives via the stream.
    pub(crate) fn cmd_respond_permission(
        &self,
        session_id: String,
        request_id: String,
        decision: tools_core::PermissionDecision,
    ) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            if let Err(e) = service
                .respond_permission(session_id, request_id, decision)
                .await
            {
                gpui.display_error(format!("{e:#}"));
            }
        });
    }

    // ========================================================================
    // Branching
    // ========================================================================

    /// Prepare editing a past message: truncates the transcript and loads
    /// the message content into the input area (via `MessageEditReady`).
    pub(crate) fn cmd_start_message_edit(&self, session_id: String, node_id: NodeId) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            match service.start_message_edit(session_id, node_id).await {
                Ok(edit) => gpui.push_event(UiEvent::MessageEditReady {
                    content: edit.content,
                    attachments: edit.attachments,
                    branch_parent_id: edit.branch_parent_id,
                    messages: edit.transcript.messages,
                    tool_results: edit.transcript.tool_results,
                }),
                Err(e) => gpui.display_error(format!("Failed to start message edit: {e:#}")),
            }
        });
    }

    pub(crate) fn cmd_switch_branch(&self, session_id: String, new_node_id: NodeId) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            match service.switch_branch(session_id.clone(), new_node_id).await {
                Ok(data) => {
                    gpui.push_event(UiEvent::SetMessages {
                        messages: data.transcript.messages,
                        session_id: Some(session_id),
                        tool_results: data.transcript.tool_results,
                    });
                    gpui.push_event(UiEvent::UpdatePlan { plan: data.plan });
                }
                Err(e) => gpui.display_error(format!("Failed to switch branch: {e:#}")),
            }
        });
    }

    /// Abort a message edit: restore the full transcript of the active path.
    pub(crate) fn cmd_cancel_message_edit(&self, session_id: String) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            match service.cancel_message_edit(session_id.clone()).await {
                Ok(transcript) => gpui.push_event(UiEvent::SetMessages {
                    messages: transcript.messages,
                    session_id: Some(session_id),
                    tool_results: transcript.tool_results,
                }),
                Err(e) => gpui.display_error(format!("Failed to cancel message edit: {e:#}")),
            }
        });
    }

    // ========================================================================
    // Worktrees
    // ========================================================================

    pub(crate) fn cmd_list_branches_and_worktrees(&self, session_id: String) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            match service
                .list_branches_and_worktrees(session_id.clone())
                .await
            {
                Ok(listing) => {
                    if gpui.is_current_session(&session_id) {
                        // Preserve the current selection (set by the switch /
                        // create commands)
                        let existing_path = gpui
                            .current_worktree_data
                            .lock()
                            .unwrap()
                            .as_ref()
                            .and_then(|d| d.current_worktree_path.clone());
                        gpui.push_event(UiEvent::UpdateWorktreeData {
                            worktrees: listing.worktrees,
                            current_worktree_path: existing_path,
                            is_git_repo: listing.is_git_repo,
                        });
                    }
                }
                Err(e) => debug!("Failed to list branches/worktrees: {e:#}"),
            }
        });
    }

    pub(crate) fn cmd_switch_worktree(
        &self,
        session_id: String,
        worktree_path: Option<PathBuf>,
        branch: Option<String>,
    ) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            match service
                .switch_worktree(session_id.clone(), worktree_path.clone(), branch)
                .await
            {
                Ok(()) => {
                    if gpui.is_current_session(&session_id) {
                        info!(
                            "Worktree switched for active session {session_id}: {worktree_path:?}"
                        );
                        gpui.update_worktree_selection(worktree_path);
                        // Refresh the full list since worktrees may have changed
                        gpui.cmd_list_branches_and_worktrees(session_id);
                    }
                }
                Err(e) => gpui.display_error(format!("Failed to switch worktree: {e:#}")),
            }
        });
    }

    pub(crate) fn cmd_create_worktree(&self, session_id: String, branch_name: String) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            match service
                .create_worktree(session_id.clone(), branch_name, None)
                .await
            {
                Ok(created) => {
                    if gpui.is_current_session(&session_id) {
                        info!(
                            "Worktree created for active session {session_id}: {:?} (branch: {})",
                            created.path, created.branch
                        );
                        gpui.update_worktree_selection(Some(created.path));
                        gpui.cmd_list_branches_and_worktrees(session_id);
                    }
                }
                Err(e) => gpui.display_error(format!("Failed to create worktree: {e:#}")),
            }
        });
    }

    /// Update the stored worktree selection, preserving the cached listing.
    fn update_worktree_selection(&self, worktree_path: Option<PathBuf>) {
        let current_data = self.current_worktree_data.lock().unwrap().clone();
        let (worktrees, is_git_repo) = current_data
            .map(|d| (d.worktrees, d.is_git_repo))
            .unwrap_or_default();
        self.push_event(UiEvent::UpdateWorktreeData {
            worktrees,
            current_worktree_path: worktree_path,
            is_git_repo,
        });
    }

    // ========================================================================
    // Projects
    // ========================================================================

    /// Add a project and connect its initial session.
    pub(crate) fn cmd_add_project(&self, name: String, path: PathBuf) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            match service.add_project(name.clone(), path).await {
                Ok(AddProjectOutcome::Added { session_id }) => {
                    info!("Project '{name}' added, initial session: {session_id}");
                    *gpui.current_session_id.lock().unwrap() = Some(session_id.clone());
                    gpui.cmd_refresh_chat_list();
                    gpui.cmd_load_session(session_id, None);
                }
                Ok(AddProjectOutcome::AlreadyExists) => {
                    info!("Project '{name}' already exists — nothing to do");
                }
                Err(e) => gpui.display_error(format!("Failed to add project: {e:#}")),
            }
        });
    }

    /// Persist a temporary project to projects.json.
    pub(crate) fn cmd_persist_project(&self, project_name: String) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.dispatch(async move {
            match service.persist_project(project_name.clone()).await {
                Ok(()) => {
                    // Update the set of persisted projects so the sidebar can
                    // remove the "pin" icon for this project; the chat-list
                    // refresh triggers the re-render.
                    gpui.persisted_projects.lock().unwrap().insert(project_name);
                    gpui.cmd_refresh_chat_list();
                }
                Err(e) => gpui.display_error(format!("Failed to persist project: {e:#}")),
            }
        });
    }
}
