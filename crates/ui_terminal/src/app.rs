use crate::{
    commands::CommandProcessor,
    input::{InputManager, KeyEventResult},
    renderer::ProductionTerminalRenderer,
    slash_popup::CommandListPopup,
    state::AppState,
    textarea::TextArea,
    tui,
    ui::TerminalUI,
};
use anyhow::Result;
use code_assistant_core::config;
use code_assistant_core::config::AgentRunConfig;
use code_assistant_core::persistence::FileSessionPersistence;
use code_assistant_core::session::manager::SessionManager;
use code_assistant_core::session::service::{AgentRuntimeOptions, SessionService};
use code_assistant_core::session::SessionConfig;
use code_assistant_core::ui::{UiEvent, UserInterface};

use crossterm::cursor::MoveTo;
use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::Mutex;
use tokio::time::Duration;
use tracing::debug;

/// Update the popup stack to reflect a new slash-prefix on the current input line.
///
/// - `query == None` → user just deleted the leading `/`; close the popup.
/// - `query == Some(q)` → ensure a [`CommandListPopup`] is on the stack and
///   forward the query so the visible rows are filtered.
async fn handle_slash_prefix_changed(
    query: Option<String>,
    app_state: &Arc<Mutex<AppState>>,
    input_manager: &mut InputManager,
) {
    let mut state = app_state.lock().await;
    match query {
        None => {
            state.popup_stack.clear();
        }
        Some(q) => {
            // If the stack is empty (or the top is not the root command list),
            // open the root command list popup. Sub-popups remain on the stack
            // and ignore the prefix change (the top-of-stack popup decides).

            if state.popup_stack.depth() == 0 {
                let skills = state.skills.clone();
                state
                    .popup_stack
                    .push(Box::new(CommandListPopup::with_skills(skills)));
            }
            state.popup_stack.set_query(&q);
        }
    }
    input_manager.popup_active = state.popup_stack.is_active();
}

/// Delete a leading `/` from the current line of `textarea`, if any. Used
/// when the user dismisses the root popup with Esc so the next keystroke
/// does not immediately reopen it.
fn delete_leading_slash(textarea: &mut TextArea) {
    let cursor = textarea.cursor();
    let text = textarea.text().to_string();
    let line_start = text[..cursor].rfind('\n').map(|i| i + 1).unwrap_or(0);
    if text[line_start..].starts_with('/') {
        textarea.replace_range(line_start..line_start + 1, "");
    }
}

/// When a popup commits a final command, the leading "/word" the user typed
/// is no longer needed in the composer (it has been consumed). Remove the
/// "/" plus everything up to the next whitespace.
fn clear_slash_command_word(textarea: &mut TextArea) {
    let cursor = textarea.cursor();
    let text = textarea.text().to_string();
    let line_start = text[..cursor].rfind('\n').map(|i| i + 1).unwrap_or(0);
    if !text[line_start..].starts_with('/') {
        return;
    }
    // Find end of the slash-word: first whitespace or end of line.
    let mut end = line_start + 1;
    for (idx, ch) in text[line_start + 1..].char_indices() {
        let abs = line_start + 1 + idx;
        if ch.is_whitespace() {
            end = abs;
            break;
        }
        end = abs + ch.len_utf8();
    }
    textarea.replace_range(line_start..end, "");
}

/// The terminal UI's session commands: thin wrappers around
/// [`SessionService`] calls that apply each typed result to the app state /
/// UI. Calls run in spawned tasks so the input loop never blocks on the
/// backend.
#[derive(Clone)]
struct Actions {
    service: SessionService,
    ui: Arc<dyn UserInterface>,
    app_state: Arc<Mutex<AppState>>,
    redraw_tx: tokio::sync::watch::Sender<()>,
}

impl Actions {
    fn display_error(&self, message: String) {
        let ui = self.ui.clone();
        tokio::spawn(async move {
            let _ = ui.send_event(UiEvent::DisplayError { message }).await;
        });
    }

    /// Whether the current session is view-only because another
    /// code-assistant instance runs its agent. Session-mutating actions are
    /// refused in that state (the local in-memory session would diverge from
    /// the one the external instance keeps writing).
    async fn refuse_if_view_only(&self) -> bool {
        let mut state = self.app_state.lock().await;
        let view_only = state
            .activity_state
            .as_ref()
            .is_some_and(|s| s.is_running_externally());
        if view_only {
            state.set_info_message(Some(
                "This session is running in another code-assistant instance — \
                 it is read-only until that agent finishes."
                    .to_string(),
            ));
            drop(state);
            let _ = self.redraw_tx.send(());
        }
        view_only
    }

    fn send_user_message(
        &self,
        session_id: String,
        message: String,
        attachments: Vec<code_assistant_core::persistence::DraftAttachment>,
    ) {
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(e) = this
                .service
                .send_user_message(session_id, message, attachments, None)
                .await
            {
                this.display_error(format!("Failed to send message: {e:#}"));
            }
        });
    }

    fn queue_user_message(
        &self,
        session_id: String,
        message: String,
        attachments: Vec<code_assistant_core::persistence::DraftAttachment>,
    ) {
        let this = self.clone();
        tokio::spawn(async move {
            match this
                .service
                .queue_user_message(session_id, message, attachments)
                .await
            {
                Ok(pending) => {
                    let _ = this
                        .ui
                        .send_event(UiEvent::UpdatePendingMessage { message: pending })
                        .await;
                }
                Err(e) => this.display_error(format!("Failed to queue message: {e:#}")),
            }
        });
    }

    fn switch_model(&self, session_id: String, model_name: String) {
        let this = self.clone();
        tokio::spawn(async move {
            if this.refuse_if_view_only().await {
                return;
            }
            match this
                .service
                .switch_model(session_id, model_name.clone())
                .await
            {
                Ok(result) => {
                    let mut state = this.app_state.lock().await;
                    state.update_current_model(Some(model_name.clone()));
                    let info = match result.warning {
                        Some(w) => format!("Switched to model: {model_name} ({w})"),
                        None => format!("Switched to model: {model_name}"),
                    };
                    state.set_info_message(Some(info));
                    drop(state);
                    let _ = this.redraw_tx.send(());
                }
                Err(e) => this.display_error(format!("{e:#}")),
            }
        });
    }

    /// Ask the running agent to stop at its next streaming checkpoint.
    fn request_stop(&self, session_id: String) {
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(e) = this.service.request_stop(session_id).await {
                tracing::debug!("Failed to request agent stop: {e:#}");
            }
        });
    }

    fn clear_context(&self, session_id: String) {
        let this = self.clone();
        tokio::spawn(async move {
            if this.refuse_if_view_only().await {
                return;
            }
            if let Err(e) = this.service.clear_context(session_id).await {
                this.display_error(format!("Failed to clear context: {e:#}"));
            }
        });
    }

    fn compact_context(&self, session_id: String) {
        let this = self.clone();
        tokio::spawn(async move {
            if this.refuse_if_view_only().await {
                return;
            }
            if let Err(e) = this.service.compact_context(session_id).await {
                this.display_error(format!("{e:#}"));
            }
        });
    }

    fn change_permission_tier(&self, session_id: String, tier: tools_core::PermissionTier) {
        let this = self.clone();
        tokio::spawn(async move {
            if this.refuse_if_view_only().await {
                return;
            }
            if let Err(e) = this.service.change_permission_tier(session_id, tier).await {
                this.display_error(format!("{e:#}"));
            }
        });
    }

    fn respond_permission(
        &self,
        session_id: String,
        request_id: String,
        decision: tools_core::PermissionDecision,
    ) {
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(e) = this
                .service
                .respond_permission(session_id, request_id, decision)
                .await
            {
                this.display_error(format!("{e:#}"));
            }
        });
    }

    fn invoke_skill(&self, session_id: String, scope: String, name: String) {
        let this = self.clone();
        tokio::spawn(async move {
            if this.refuse_if_view_only().await {
                return;
            }
            if let Err(e) = this.service.invoke_skill(session_id, scope, name).await {
                this.display_error(format!("{e:#}"));
            }
        });
    }

    /// Fetch the session list and publish it to the UI.
    fn refresh_chat_list(&self) {
        let this = self.clone();
        tokio::spawn(async move {
            match this.service.list_sessions().await {
                Ok(sessions) => {
                    let _ = this
                        .ui
                        .send_event(UiEvent::UpdateChatList { sessions })
                        .await;
                }
                Err(e) => this.display_error(format!("Failed to list sessions: {e:#}")),
            }
        });
    }

    /// Fetch the skill catalog for the `/skill` picker and cache it.
    fn refresh_skills(&self, session_id: String) {
        let this = self.clone();
        tokio::spawn(async move {
            match this.service.list_skills(session_id).await {
                Ok(skills) => {
                    this.app_state.lock().await.skills = skills;
                }
                Err(e) => {
                    debug!("Failed to list skills: {e:#}");
                }
            }
        });
    }
}

/// Run the side-effects for a [`CommandResult`] produced by either an inline
/// `/cmd<Enter>` submission or a popup commit.
async fn handle_command_result(
    cmd: crate::commands::CommandResult,
    app_state: &Arc<Mutex<AppState>>,
    renderer: &Arc<Mutex<ProductionTerminalRenderer>>,
    actions: &Actions,
) {
    use crate::commands::CommandResult;
    match cmd {
        CommandResult::Continue => {}
        CommandResult::Help(_) => {
            let processor = CommandProcessor::new().ok();
            let text = processor
                .map(|p| match p.process_command("/help") {
                    CommandResult::Help(t) => t,
                    _ => String::new(),
                })
                .unwrap_or_default();
            app_state.lock().await.set_info_message(Some(text));
        }
        CommandResult::ListModels => {
            if let Ok(p) = CommandProcessor::new() {
                app_state
                    .lock()
                    .await
                    .set_info_message(Some(p.get_models_list()));
            }
        }
        CommandResult::ListProviders => {
            if let Ok(p) = CommandProcessor::new() {
                app_state
                    .lock()
                    .await
                    .set_info_message(Some(p.get_providers_list()));
            }
        }
        CommandResult::SwitchModel(model_name) => {
            let session_id = {
                let state = app_state.lock().await;
                state.current_session_id.clone()
            };
            if let Some(session_id) = session_id {
                actions.switch_model(session_id, model_name);
            } else {
                app_state
                    .lock()
                    .await
                    .set_info_message(Some("No active session to switch model".to_string()));
            }
        }
        CommandResult::ShowCurrentModel => {
            let current_model = app_state.lock().await.current_model.clone();
            let message = match current_model {
                Some(model) => format!("Current model: {model}"),
                None => "No model selected".to_string(),
            };
            app_state.lock().await.set_info_message(Some(message));
        }
        CommandResult::TogglePlan => {
            let (plan_state, expanded, overlay_active) = {
                let mut state = app_state.lock().await;
                let expanded = state.toggle_plan_expanded();
                (state.plan.clone(), expanded, state.is_overlay_active())
            };
            let mut renderer_guard = renderer.lock().await;
            if let Some(plan_state) = plan_state {
                renderer_guard.set_plan_state(Some(plan_state));
            }
            renderer_guard.set_plan_expanded(expanded);
            renderer_guard.set_overlay_active(overlay_active);
        }
        CommandResult::ClearContext => {
            let session_id = app_state.lock().await.current_session_id.clone();
            if let Some(session_id) = session_id {
                actions.clear_context(session_id);
            }
        }
        CommandResult::CompactContext => {
            let session_id = app_state.lock().await.current_session_id.clone();
            if let Some(session_id) = session_id {
                actions.compact_context(session_id);
            }
        }

        CommandResult::OpenSkillPicker => {
            // Open the skill picker built from the cached catalog. If no skills
            // are available, show an informational message instead.
            let mut state = app_state.lock().await;
            if state.skills.is_empty() {
                state.set_info_message(Some(
                    "No skills are available for this session.".to_string(),
                ));
            } else {
                let skills = state.skills.clone();
                state.popup_stack.push(Box::new(
                    crate::slash_popup::SkillPickerPopup::from_entries(skills),
                ));
            }
        }
        CommandResult::InvokeSkill { scope, name } => {
            let session_id = app_state.lock().await.current_session_id.clone();
            let Some(session_id) = session_id else {
                app_state
                    .lock()
                    .await
                    .set_info_message(Some("No active session to activate a skill".to_string()));
                return;
            };

            // Resolve the scope token: use the explicit one from the picker, or
            // look it up in the cached catalog by name (inline `/skill <name>`).
            let resolved_scope = match scope {
                Some(scope) => Some(scope),
                None => app_state
                    .lock()
                    .await
                    .skills
                    .iter()
                    .find(|s| s.name == name)
                    .map(|s| s.scope_token.clone()),
            };

            match resolved_scope {
                Some(scope) => {
                    app_state
                        .lock()
                        .await
                        .set_info_message(Some(format!("Activating skill: {name}")));
                    actions.invoke_skill(session_id, scope, name);
                }
                None => {
                    app_state
                        .lock()
                        .await
                        .set_info_message(Some(format!("No skill named '{name}' was found")));
                }
            }
        }
        CommandResult::ShowPermissionTier => {
            let mut state = app_state.lock().await;
            let message = match state.current_permission_tier {
                Some(tier) => format!("Permission tier: {tier:?}"),
                None => "No active session".to_string(),
            };
            state.set_info_message(Some(message));
        }
        CommandResult::SetPermissionTier(tier) => {
            let session_id = app_state.lock().await.current_session_id.clone();
            if let Some(session_id) = session_id {
                actions.change_permission_tier(session_id, tier);
            }
        }
        CommandResult::RespondPermission {
            request_id,
            decision,
        } => {
            let (session_id, request_id) = {
                let state = app_state.lock().await;
                (
                    state.current_session_id.clone(),
                    // Slash commands pass no id and answer the oldest request.
                    request_id.or_else(|| {
                        state
                            .pending_permission_requests
                            .first()
                            .map(|r| r.request_id.clone())
                    }),
                )
            };
            match (session_id, request_id) {
                (Some(session_id), Some(request_id)) => {
                    actions.respond_permission(session_id, request_id, decision);
                }
                _ => {
                    app_state
                        .lock()
                        .await
                        .set_info_message(Some("No pending permission request".to_string()));
                }
            }
        }
        CommandResult::InvalidCommand(error) => {
            app_state
                .lock()
                .await
                .set_info_message(Some(format!("Error: {error}")));
        }
    }
}

/// Main event loop for handling terminal events
async fn event_loop(
    mut input_manager: InputManager,
    renderer: Arc<Mutex<ProductionTerminalRenderer>>,
    app_state: Arc<Mutex<AppState>>,
    cancel_flag: Arc<AtomicBool>,
    actions: Actions,
    mut tui: tui::Tui,
    mut redraw_rx: tokio::sync::watch::Receiver<()>,
) -> Result<()> {
    let mut event_stream = EventStream::new();
    let mut needs_redraw = true; // Draw initial frame

    loop {
        // === PHASE 1: Draw if needed ===
        if needs_redraw {
            {
                let mut renderer_guard = renderer.lock().await;
                let mut state = app_state.lock().await;

                // Sync info message from state to renderer
                if let Some(ref info_msg) = state.info_message {
                    renderer_guard.set_info(info_msg.clone());
                } else {
                    renderer_guard.clear_info();
                }

                if state.plan_dirty {
                    renderer_guard.set_plan_state(state.plan.clone());
                    state.plan_dirty = false;
                }
                renderer_guard.set_plan_expanded(state.plan_expanded);
                renderer_guard.set_overlay_active(state.is_overlay_active());

                // Sync popup snapshot from the popup stack.
                let snap = if state.popup_stack.is_active() {
                    let top = state.popup_stack.top().unwrap();
                    Some(crate::renderer::PopupSnapshot {
                        breadcrumb: state
                            .popup_stack
                            .breadcrumb()
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                        rows: top.rows().to_vec(),
                        selected: top.selected(),
                        stack_depth: state.popup_stack.depth(),
                    })
                } else {
                    None
                };
                renderer_guard.set_popup_snapshot(snap);

                drop(state); // Release the lock before rendering

                let screen_size = tui.size()?;

                // Prepare renderer state (streaming tick, flush finalized messages)
                renderer_guard.prepare(screen_size.width, screen_size.height);

                // Drain pending history lines and insert them into scrollback
                let pending_lines = renderer_guard.drain_pending_history_lines();
                if !pending_lines.is_empty() {
                    tui.insert_history_lines(pending_lines);
                }

                // Compute desired viewport height and draw
                let desired_height = renderer_guard
                    .desired_viewport_height(&input_manager.textarea, screen_size.width);
                tui.draw(desired_height, |frame| {
                    renderer_guard.paint(frame, &input_manager.textarea);
                })?;
            }
            needs_redraw = false;
        }

        // === PHASE 2: Determine animation timer ===
        let animation_delay = {
            let renderer_guard = renderer.lock().await;
            if renderer_guard.needs_animation_timer() {
                Duration::from_millis(50)
            } else {
                // Effectively infinite - no animation needed
                Duration::from_secs(86400)
            }
        };

        // === PHASE 3: Wait for any wake source ===
        tokio::select! {
            maybe_event = event_stream.next() => {
                match maybe_event {
                    Some(Ok(event)) => match event {
                        Event::Key(key_event) => {
                            // Permission prompts push popups from the backend
                            // event task; resync routing before each key so
                            // Up/Down/Enter reach an asynchronously opened popup.
                            input_manager.popup_active =
                                app_state.lock().await.popup_stack.is_active();
                            let key_result = input_manager.handle_key_event(key_event);

                            match key_result {
                                KeyEventResult::Quit => {
                                    break;
                                }
                                KeyEventResult::Escape => {
                                    // Check if there's an error to dismiss first
                                    let has_error = {
                                        let renderer_guard = renderer.lock().await;
                                        renderer_guard.has_error()
                                    };

                                    let has_info = {
                                        let state = app_state.lock().await;
                                        state.info_message.is_some()
                                    };

                                    if has_error {
                                        // Clear the error
                                        let mut renderer_guard = renderer.lock().await;
                                        renderer_guard.clear_error();
                                    } else if has_info {
                                        // Clear the info message
                                        let mut state = app_state.lock().await;
                                        state.set_info_message(None);
                                    } else {
                                        // Capture current activity/session in one lock to reduce lag
                                        let (activity_state, current_session_id) = {
                                            let state = app_state.lock().await;
                                            (
                                                state.activity_state.clone(),
                                                state.current_session_id.clone(),
                                            )
                                        };

                                        if let Some(session_id) = current_session_id {
                                            cancel_flag.store(true, Ordering::SeqCst);
                                            debug!(
                                                "Escape pressed - requesting stop for session {} (state: {:?})",
                                                session_id, activity_state
                                            );

                                            let mut state = app_state.lock().await;
                                            if activity_state.as_ref().is_some_and(|s| s.is_terminal()) {
                                                state.set_info_message(Some(
                                                    "No agent is currently running.".to_string(),
                                                ));
                                            } else if activity_state
                                                .as_ref()
                                                .is_some_and(|s| s.is_running_externally())
                                            {
                                                state.set_info_message(Some(
                                                    "The agent runs in another code-assistant \
                                                     instance and cannot be cancelled from here."
                                                        .to_string(),
                                                ));
                                            } else {
                                                actions.request_stop(session_id.clone());
                                                state.set_info_message(Some(
                                                    "Cancellation requested...".to_string(),
                                                ));
                                                debug!("Cancellation requested for session {}", session_id);
                                            }
                                        }
                                    }
                                }
                                KeyEventResult::SendMessage {
                                    message,
                                    attachments,
                                } => {
                                    let current_session_id = {
                                        let state = app_state.lock().await;
                                        state.current_session_id.clone()
                                    };

                                    if let Some(session_id) = current_session_id {
                                        let activity_state = {
                                            let state = app_state.lock().await;
                                            state.activity_state.clone()
                                        };

                                        if activity_state
                                            .as_ref()
                                            .is_some_and(|s| s.is_running_externally())
                                        {
                                            // The session is view-only while another
                                            // instance runs it: refuse the submit and
                                            // restore the composer so nothing typed
                                            // is lost.
                                            input_manager.textarea.insert_str(&message);
                                            input_manager.attachments = attachments;
                                            app_state.lock().await.set_info_message(Some(
                                                "This session is running in another \
                                                 code-assistant instance — input is \
                                                 disabled until it finishes."
                                                    .to_string(),
                                            ));
                                        } else if activity_state
                                            .as_ref()
                                            .is_none_or(|s| s.is_terminal())
                                        {
                                            // No agent running: start a new turn.
                                            cancel_flag.store(false, Ordering::SeqCst);
                                            actions.send_user_message(
                                                session_id,
                                                message,
                                                attachments,
                                            );
                                        } else {
                                            // Local agent running: queue the message.
                                            actions.queue_user_message(
                                                session_id,
                                                message,
                                                attachments,
                                            );
                                        }
                                    }
                                }
                                KeyEventResult::Continue => {
                                    // Input may have changed (cursor, text), redraw below
                                }
                                KeyEventResult::ShowInfo(info_text) => {
                                    // Display info message in the UI
                                    let mut state = app_state.lock().await;
                                    state.set_info_message(Some(info_text));
                                }
                                KeyEventResult::SwitchModel(model_name) => {
                                    // Handle model switching
                                    let current_session_id = {
                                        let state = app_state.lock().await;
                                        state.current_session_id.clone()
                                    };

                                    if let Some(session_id) = current_session_id {
                                        // State and info message are updated when
                                        // the switch succeeds (see Actions).
                                        actions.switch_model(session_id, model_name);
                                    } else {
                                        let mut state = app_state.lock().await;
                                        state.set_info_message(Some(
                                            "No active session to switch model".to_string(),
                                        ));
                                    }
                                }
                                KeyEventResult::ShowCurrentModel => {
                                    let current_model = {
                                        let state = app_state.lock().await;
                                        state.current_model.clone()
                                    };

                                    let message = match current_model {
                                        Some(model) => format!("Current model: {model}"),
                                        None => "No model selected".to_string(),
                                    };

                                    let mut state = app_state.lock().await;
                                    state.set_info_message(Some(message));
                                }
                                KeyEventResult::TogglePlan => {
                                    let (plan_state, expanded, overlay_active) = {
                                        let mut state = app_state.lock().await;
                                        let expanded = state.toggle_plan_expanded();
                                        (state.plan.clone(), expanded, state.is_overlay_active())
                                    };

                                    let mut renderer_guard = renderer.lock().await;
                                    if let Some(plan_state) = plan_state {
                                        renderer_guard.set_plan_state(Some(plan_state));
                                    } else {
                                        debug!("TogglePlan invoked with no plan available; renderer state unchanged");
                                    }
                                    renderer_guard.set_plan_expanded(expanded);
                                    renderer_guard.set_overlay_active(overlay_active);
                                }
                                KeyEventResult::ClearContext => {
                                    let current_session_id = {
                                        let state = app_state.lock().await;
                                        state.current_session_id.clone()
                                    };
                                    if let Some(session_id) = current_session_id {
                                        actions.clear_context(session_id);
                                    }
                                }
                                KeyEventResult::CompactContext => {
                                    let current_session_id = {
                                        let state = app_state.lock().await;
                                        state.current_session_id.clone()
                                    };
                                    if let Some(session_id) = current_session_id {
                                        actions.compact_context(session_id);
                                    }
                                }

                                KeyEventResult::ShowPermissionTier => {
                                    let mut state = app_state.lock().await;
                                    let message = match state.current_permission_tier {
                                        Some(tier) => format!("Permission tier: {tier:?}"),
                                        None => "No active session".to_string(),
                                    };
                                    state.set_info_message(Some(message));
                                }
                                KeyEventResult::SetPermissionTier(tier) => {
                                    let current_session_id = {
                                        let state = app_state.lock().await;
                                        state.current_session_id.clone()
                                    };
                                    if let Some(session_id) = current_session_id {
                                        actions.change_permission_tier(session_id, tier);
                                    }
                                }
                                KeyEventResult::RespondPermission {
                                    request_id,
                                    decision,
                                } => {
                                    handle_command_result(
                                        crate::commands::CommandResult::RespondPermission {
                                            request_id,
                                            decision,
                                        },
                                        &app_state,
                                        &renderer,
                                        &actions,
                                    )
                                    .await;
                                }

                                KeyEventResult::OpenSkillPicker => {
                                    // Inline `/skill` (popup not active): reuse the
                                    // command-result handler to open the picker.
                                    handle_command_result(
                                        crate::commands::CommandResult::OpenSkillPicker,
                                        &app_state,
                                        &renderer,
                                        &actions,
                                    )
                                    .await;
                                }
                                KeyEventResult::InvokeSkill { scope, name } => {
                                    handle_command_result(
                                        crate::commands::CommandResult::InvokeSkill { scope, name },
                                        &app_state,
                                        &renderer,
                                        &actions,
                                    )
                                    .await;
                                }
                                KeyEventResult::SlashPrefixChanged(query) => {
                                    handle_slash_prefix_changed(
                                        query,
                                        &app_state,
                                        &mut input_manager,
                                    )
                                    .await;
                                }
                                KeyEventResult::PopupQueryChanged(text) => {
                                    let mut state = app_state.lock().await;
                                    // For the root popup the user typed "/cl",
                                    // so the query is the part after the leading "/".
                                    // For sub-popups the composer is the query verbatim.
                                    let query: String = if state.popup_stack.depth() == 1
                                        && text.starts_with('/')
                                    {
                                        text[1..].to_string()
                                    } else {
                                        text
                                    };
                                    state.popup_stack.set_query(&query);
                                    input_manager.popup_active =
                                        state.popup_stack.is_active();
                                }
                                KeyEventResult::PopupKey(key) => {
                                    let outcome = {
                                        let mut state = app_state.lock().await;
                                        // Capture root-Esc *before* dispatch so we can also
                                        // delete the leading "/" from the composer when the
                                        // user dismisses the root popup.
                                        // Permission prompts are not opened by a
                                        // typed "/", so dismissing one must not
                                        // eat a leading slash from the composer.
                                        let top_is_permission_prompt = state
                                            .popup_stack
                                            .top()
                                            .is_some_and(|p| p.permission_request_id().is_some());
                                        let was_root_esc = matches!(
                                            key.code,
                                            crossterm::event::KeyCode::Esc
                                        ) && state.popup_stack.depth() == 1
                                            && !top_is_permission_prompt;
                                        let depth_before = state.popup_stack.depth();
                                        let result = state.popup_stack.handle_key(key);
                                        let depth_after = state.popup_stack.depth();
                                        let still_active = state.popup_stack.is_active();
                                        (
                                            result,
                                            was_root_esc,
                                            still_active,
                                            depth_before,
                                            depth_after,
                                            top_is_permission_prompt,
                                        )
                                    };
                                    let (
                                        committed,
                                        was_root_esc,
                                        still_active,
                                        depth_before,
                                        depth_after,
                                        top_was_permission_prompt,
                                    ) = outcome;
                                    input_manager.popup_active = still_active;

                                    if was_root_esc && !still_active {
                                        // Drop a leading "/" from the current composer line so
                                        // the popup does not re-open on the next keystroke.
                                        delete_leading_slash(&mut input_manager.textarea);
                                    }

                                    // If a sub-popup was just pushed, clear the composer so
                                    // the user can type a fresh query for the new popup
                                    // (e.g. "/mo<Enter>" -> empty composer that filters models).
                                    if depth_after > depth_before {
                                        input_manager.clear();
                                        // Reset the new popup's query to empty.
                                        let mut state = app_state.lock().await;
                                        state.popup_stack.set_query("");
                                    }

                                    if let Some(cmd) = committed {
                                        // The popup committed a final command; clear the
                                        // composer line (the slash word) and dispatch the
                                        // command via the same path as inline /commands.
                                        // Permission prompts were not opened by a typed
                                        // "/word", so their commit leaves the composer alone.
                                        if !top_was_permission_prompt {
                                            clear_slash_command_word(&mut input_manager.textarea);
                                        }
                                        handle_command_result(cmd, &app_state, &renderer, &actions)
                                            .await;
                                    }
                                }
                            }
                            needs_redraw = true;
                        }
                        Event::Paste(pasted) => {
                            // Many terminals convert newlines to \r when pasting;
                            // normalize before processing.
                            let pasted = pasted.replace('\r', "\n");
                            input_manager.handle_paste(pasted);
                            // Update autocomplete: pasting may introduce or clear a slash prefix.
                            let prefix = input_manager.slash_prefix();
                            handle_slash_prefix_changed(prefix, &app_state, &mut input_manager)
                                .await;
                            needs_redraw = true;
                        }
                        Event::Resize(_, _) => {
                            needs_redraw = true;
                        }
                        _ => {}
                    },
                    Some(Err(e)) => {
                        return Err(e.into());
                    }
                    None => {
                        // Event stream ended
                        break;
                    }
                }
            }

            _ = redraw_rx.changed() => {
                needs_redraw = true;
            }

            _ = tokio::time::sleep(animation_delay) => {
                needs_redraw = true;
            }
        }
    }

    // Move cursor below the viewport so post-exit output (e.g. "Goodbye!")
    // appears below the UI instead of overlapping the composer area.
    let viewport = tui.terminal.viewport_area;
    crossterm::execute!(std::io::stdout(), MoveTo(0, viewport.bottom()))?;

    Ok(())
}

#[derive(Default)]
pub struct TerminalTuiApp {}

impl TerminalTuiApp {
    pub async fn run(
        &self,
        config: &AgentRunConfig,
        command_executor_factory: code_assistant_core::session::service::CommandExecutorFactory,
    ) -> Result<()> {
        let app_state = Arc::new(Mutex::new(AppState::new()));
        let root_path = config.path.canonicalize()?;

        // Create session persistence
        let session_persistence = FileSessionPersistence::new();

        // Setup agent configuration

        let session_config_template = SessionConfig {
            init_path: Some(root_path.clone()),
            initial_project: root_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("unknown")
                .to_string(),
            tool_syntax: config.tool_syntax,
            use_diff_blocks: config.use_diff_format,
            sandbox_policy: config.sandbox_policy.clone(),
            ..SessionConfig::default()
        };

        // Create session manager. The registry provider rebuilds the tool
        // set from the current configuration at the start of every agent run,
        // so settings edits (e.g. adding an MCP server) apply on the next run
        // without restarting.
        let events = code_assistant_core::session::event_stream::EventStream::new();
        let registry_provider = code_assistant_core::tools::ConfigToolRegistry::new();
        let mut session_manager = SessionManager::new(
            session_persistence,
            session_config_template,
            config.model.clone(),
            registry_provider.current().await,
            events.clone(),
        );
        session_manager.set_tool_registry_provider(registry_provider.as_provider());
        let multi_session_manager = Arc::new(Mutex::new(session_manager));

        // Create terminal UI and wrap as UserInterface
        let terminal_ui = TerminalUI::new_with_state(app_state.clone());
        let ui: Arc<dyn UserInterface> = Arc::new(terminal_ui.clone());

        // Setup UI event channel for display fragments
        let (ui_event_tx, ui_event_rx) =
            async_channel::unbounded::<code_assistant_core::ui::UiEvent>();
        terminal_ui.set_event_sender(ui_event_tx);

        // Create the session command service and spawn its worker. This
        // frontend consumes the broadcast stream (see the bridge task below).
        let (service, service_worker) = SessionService::new(
            multi_session_manager.clone(),
            Arc::new(AgentRuntimeOptions {
                record_path: config.record.clone(),
                playback_path: config.playback.clone(),
                fast_playback: config.fast_playback,
                command_executor_factory,
            }),
            events,
        );
        let backend_task = tokio::spawn(service_worker);

        // Wakeup scheduler: lets agents arm timed continuations of their
        // session (schedule_wakeup tool).
        {
            let mut manager = multi_session_manager.lock().await;
            let sleep_inhibitor = manager.sleep_inhibitor();
            manager.set_wakeup_handle(code_assistant_core::session::spawn_wakeup_scheduler(
                service.clone(),
                Some(sleep_inhibitor),
            ));
        }

        // Bridge: subscribe to the core→UI broadcast stream and feed the
        // terminal's rendering pipeline. Single-session app, so everything
        // scoped to the current session (or app-scoped) passes.
        {
            let terminal_ui = terminal_ui.clone();
            let app_state = app_state.clone();
            let service_for_bridge = service.clone();
            let mut subscription = service.subscribe();
            tokio::spawn(async move {
                use code_assistant_core::session::instance::SessionActivityState;
                use code_assistant_core::session::{EventPayload, StreamError};
                loop {
                    match subscription.recv().await {
                        Ok(event) => {
                            let current = app_state.lock().await.current_session_id.clone();
                            let relevant =
                                event.session_id.is_none() || event.session_id == current;
                            if !relevant {
                                continue;
                            }
                            match event.payload {
                                EventPayload::Fragment(fragment) => {
                                    let _ = terminal_ui.display_fragment(&fragment);
                                }
                                EventPayload::Ui(ui_event) => {
                                    // Drive the rate-limit spinner from activity
                                    // transitions (used to be trait callbacks).
                                    if let UiEvent::UpdateSessionActivityState {
                                        activity_state,
                                        ..
                                    } = &ui_event
                                    {
                                        match activity_state {
                                            SessionActivityState::RateLimited {
                                                seconds_remaining,
                                            } => terminal_ui.notify_rate_limit(*seconds_remaining),
                                            _ => terminal_ui.clear_rate_limit(),
                                        }
                                    }
                                    let _ = terminal_ui.send_event(ui_event).await;
                                }
                            }
                        }
                        Err(StreamError::Lagged { missed }) => {
                            tracing::warn!("Event stream lagged ({missed} missed) — resyncing");
                            let current = app_state.lock().await.current_session_id.clone();
                            if let Some(session_id) = current {
                                if let Ok(snapshot) =
                                    service_for_bridge.load_session(session_id, None).await
                                {
                                    for event in snapshot.connect_events() {
                                        let _ = terminal_ui.send_event(event).await;
                                    }
                                }
                            }
                        }
                        Err(StreamError::Closed) => break,
                    }
                }
            });
        }

        // Create the redraw notification channel early so `Actions` can wake
        // the event loop after async state updates.
        let (redraw_tx, redraw_rx) = tokio::sync::watch::channel::<()>(());

        let actions = Actions {
            service: service.clone(),
            ui: ui.clone(),
            app_state: app_state.clone(),
            redraw_tx: redraw_tx.clone(),
        };

        // Determine which session to use and load it
        let mut session_id = None;

        // First, try to load existing session if continuing
        if config.continue_task {
            let latest_session_id = {
                let manager = multi_session_manager.lock().await;
                manager.get_latest_session_id().unwrap_or(None)
            };

            if let Some(existing_session_id) = latest_session_id {
                debug!("Continuing from latest session: {}", existing_session_id);
                match service
                    .load_session(existing_session_id.clone(), None)
                    .await
                {
                    Ok(snapshot) => {
                        for event in snapshot.connect_events() {
                            let _ = terminal_ui.send_event(event).await;
                        }
                        session_id = Some(existing_session_id);
                    }
                    Err(e) => {
                        // Fall through to creating a fresh session
                        debug!("Failed to continue session {existing_session_id}: {e:#}");
                    }
                }
            } else {
                debug!("No previous session found");
            }
        }

        // Create new session if we don't have one yet
        if session_id.is_none() {
            debug!("Creating new session");
            let new_session_id = service.create_session(None, None).await?;
            debug!("Created new session: {}", new_session_id);
            let snapshot = service.load_session(new_session_id.clone(), None).await?;
            for event in snapshot.connect_events() {
                let _ = terminal_ui.send_event(event).await;
            }
            session_id = Some(new_session_id);
        }

        let session_id = session_id.expect("Session ID should be set at this point");

        debug!("Terminal TUI connected to session: {}", session_id);

        // Immediately set current_session_id so first Enter can send
        {
            let mut state = app_state.lock().await;
            state.current_session_id = Some(session_id.clone());
        }

        // Kick off a session list refresh (optional but useful)
        actions.refresh_chat_list();

        // Fetch the skill catalog for the `/skill` picker.
        actions.refresh_skills(session_id.clone());

        // Start the filesystem watcher for cross-instance awareness, so a
        // session that another code-assistant instance appends to stays in
        // sync here (e.g. `--continue` on a session streamed elsewhere).
        let watcher_session_ref: Arc<std::sync::Mutex<Option<String>>> =
            Arc::new(std::sync::Mutex::new(Some(session_id.clone())));
        let (watcher_tx, watcher_rx) = async_channel::bounded::<UiEvent>(64);
        let _session_watcher = match code_assistant_core::session::watcher::SessionWatcher::start(
            &FileSessionPersistence::new(),
            watcher_tx,
            watcher_session_ref,
        ) {
            Ok(watcher) => {
                debug!("Filesystem session watcher started (terminal mode)");
                Some(watcher)
            }
            Err(e) => {
                debug!("Failed to start filesystem session watcher: {e}");
                None
            }
        };
        {
            let service = service.clone();
            let terminal_ui = terminal_ui.clone();
            let our_session_id = session_id.clone();
            tokio::spawn(async move {
                while let Ok(event) = watcher_rx.recv().await {
                    match event {
                        UiEvent::RefreshCurrentSession { session_id } => {
                            // The resulting AppendMessages/UpdatePlan events
                            // arrive via the broadcast stream (bridge task).
                            if let Err(e) = service.refresh_session(session_id).await {
                                debug!("Watcher-triggered refresh failed: {e:#}");
                            }
                        }
                        UiEvent::UpdateSessionActivityState { ref session_id, .. }
                            if *session_id == our_session_id =>
                        {
                            let _ = terminal_ui.send_event(event).await;
                        }
                        // Chat list and config changes have no terminal UI.
                        _ => {}
                    }
                }
            });
        }

        // Spawn a background task to process UI events from display fragments
        {
            let terminal_ui_clone = terminal_ui.clone();
            tokio::spawn(async move {
                while let Ok(event) = ui_event_rx.recv().await {
                    let _ = terminal_ui_clone.send_event(event).await;
                }
            });
        }

        // Flush stdout to ensure instructions are displayed
        std::io::Write::flush(&mut std::io::stdout())?;

        // Initialize components
        let input_manager = InputManager::new();
        let renderer = ProductionTerminalRenderer::new()?;

        // Initialize the Tui (raw mode, custom terminal, panic hook)
        let tui = tui::init()?;

        let renderer = Arc::new(Mutex::new(renderer));

        // Bind renderer to UI for message printing and input redraws
        terminal_ui.set_renderer_async(renderer.clone()).await;

        terminal_ui.set_redraw_sender(redraw_tx.clone());

        // Display welcome banner with project info
        {
            let mut renderer_guard = renderer.lock().await;

            // Determine if this is a configured (persistent) project
            let is_configured_project = config::load_projects()
                .map(|projects| projects.values().any(|p| p.path == root_path))
                .unwrap_or(false);

            // Shorten path by replacing home directory with ~
            let display_path = if let Some(home) = dirs::home_dir() {
                if let Ok(suffix) = root_path.strip_prefix(&home) {
                    format!("~/{}", suffix.display())
                } else {
                    root_path.display().to_string()
                }
            } else {
                root_path.display().to_string()
            };

            let banner_lines =
                super::welcome_banner::welcome_banner_lines(&display_path, !is_configured_project);
            renderer_guard.add_styled_history_lines(banner_lines);
        }

        // Send initial task if provided
        if let Some(task) = &config.task {
            actions.send_user_message(session_id.clone(), task.clone(), Vec::new());
        }

        // Start main event loop in a separate task
        let event_loop_handle = tokio::spawn(event_loop(
            input_manager,
            renderer.clone(),
            app_state,
            terminal_ui.cancel_flag.clone(),
            actions,
            tui,
            redraw_rx,
        ));

        // Wait for the event loop to finish (Ctrl+C or event stream end)
        let loop_result: Result<()> = match event_loop_handle.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(e.into()),
        };

        // Restore terminal state (disable raw mode)
        let cleanup_result = tui::restore();

        // Cancel the backend task
        backend_task.abort();

        if let Err(cleanup_error) = cleanup_result {
            if loop_result.is_ok() {
                return Err(cleanup_error.into());
            }
            tracing::warn!(
                "Terminal cleanup failed after loop error: {}",
                cleanup_error
            );
        }

        loop_result?;

        println!("\nGoodbye!");
        Ok(())
    }
}
