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
use code_assistant_core::backend::{
    handle_backend_events, BackendEvent, BackendResponse, BackendRuntimeOptions,
};
use code_assistant_core::config;
use code_assistant_core::config::AgentRunConfig;
use code_assistant_core::persistence::FileSessionPersistence;
use code_assistant_core::session::manager::SessionManager;
use code_assistant_core::session::SessionConfig;
use code_assistant_core::ui::UserInterface;

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
                state.popup_stack.push(Box::new(CommandListPopup::new()));
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

/// Run the side-effects for a [`CommandResult`] produced by either an inline
/// `/cmd<Enter>` submission or a popup commit. Returns an optional
/// [`BackendEvent`] the caller should send.
async fn handle_command_result(
    cmd: crate::commands::CommandResult,
    app_state: &Arc<Mutex<AppState>>,
    renderer: &Arc<Mutex<ProductionTerminalRenderer>>,
    backend_event_tx: &async_channel::Sender<BackendEvent>,
) -> Option<BackendEvent> {
    use crate::commands::CommandResult;
    match cmd {
        CommandResult::Continue => None,
        CommandResult::Help(_) => {
            let processor = CommandProcessor::new().ok();
            let text = processor
                .map(|p| match p.process_command("/help") {
                    CommandResult::Help(t) => t,
                    _ => String::new(),
                })
                .unwrap_or_default();
            app_state.lock().await.set_info_message(Some(text));
            None
        }
        CommandResult::ListModels => {
            if let Ok(p) = CommandProcessor::new() {
                app_state
                    .lock()
                    .await
                    .set_info_message(Some(p.get_models_list()));
            }
            None
        }
        CommandResult::ListProviders => {
            if let Ok(p) = CommandProcessor::new() {
                app_state
                    .lock()
                    .await
                    .set_info_message(Some(p.get_providers_list()));
            }
            None
        }
        CommandResult::SwitchModel(model_name) => {
            let session_id = {
                let state = app_state.lock().await;
                state.current_session_id.clone()
            };
            if let Some(session_id) = session_id {
                let mut state = app_state.lock().await;
                state.update_current_model(Some(model_name.clone()));
                state.set_info_message(Some(format!("Switched to model: {model_name}")));
                Some(BackendEvent::SwitchModel {
                    session_id,
                    model_name,
                })
            } else {
                app_state
                    .lock()
                    .await
                    .set_info_message(Some("No active session to switch model".to_string()));
                None
            }
        }
        CommandResult::ShowCurrentModel => {
            let current_model = app_state.lock().await.current_model.clone();
            let message = match current_model {
                Some(model) => format!("Current model: {model}"),
                None => "No model selected".to_string(),
            };
            app_state.lock().await.set_info_message(Some(message));
            None
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
            None
        }
        CommandResult::ClearContext => {
            let session_id = app_state.lock().await.current_session_id.clone();
            session_id.map(|session_id| BackendEvent::ClearContext { session_id })
        }
        CommandResult::CompactContext => {
            let session_id = app_state.lock().await.current_session_id.clone();
            session_id.map(|session_id| BackendEvent::CompactContext { session_id })
        }
        CommandResult::InvalidCommand(error) => {
            app_state
                .lock()
                .await
                .set_info_message(Some(format!("Error: {error}")));
            // Suppress unused warning in this branch.
            let _ = backend_event_tx;
            None
        }
    }
}

/// Main event loop for handling terminal events
async fn event_loop(
    mut input_manager: InputManager,
    renderer: Arc<Mutex<ProductionTerminalRenderer>>,
    app_state: Arc<Mutex<AppState>>,
    cancel_flag: Arc<AtomicBool>,
    backend_event_tx: async_channel::Sender<BackendEvent>,
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
                                                "Escape pressed - cancellation flag set for session {} (state: {:?})",
                                                session_id, activity_state
                                            );


                                            let mut state = app_state.lock().await;
                                            if activity_state.as_ref().is_some_and(|s| s.is_terminal()) {
                                                state.set_info_message(Some(
                                                    "No agent is currently running.".to_string(),
                                                ));
                                            } else {
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


                                        let event = match activity_state {
                                            Some(ref s) if s.is_terminal() => {
                                                cancel_flag.store(false, Ordering::SeqCst);
                                                BackendEvent::SendUserMessage {
                                                    session_id,
                                                    message,
                                                    attachments,
                                                    branch_parent_id: None, // Terminal UI doesn't support branching yet
                                                }
                                            }
                                            None => {
                                                cancel_flag.store(false, Ordering::SeqCst);
                                                BackendEvent::SendUserMessage {
                                                    session_id,
                                                    message,
                                                    attachments,
                                                    branch_parent_id: None, // Terminal UI doesn't support branching yet
                                                }
                                            }
                                            _ => BackendEvent::QueueUserMessage {
                                                session_id,
                                                message,
                                                attachments,
                                            },
                                        };

                                        let _ = backend_event_tx.send(event).await;
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
                                        let event = BackendEvent::SwitchModel {
                                            session_id,
                                            model_name: model_name.clone(),
                                        };

                                        let _ = backend_event_tx.send(event).await;

                                        // Update state
                                        let mut state = app_state.lock().await;
                                        state.update_current_model(Some(model_name.clone()));
                                        state.set_info_message(Some(format!(
                                            "Switched to model: {model_name}",
                                        )));
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
                                        let _ = backend_event_tx
                                            .send(BackendEvent::ClearContext { session_id })
                                            .await;
                                    }
                                }
                                KeyEventResult::CompactContext => {
                                    let current_session_id = {
                                        let state = app_state.lock().await;
                                        state.current_session_id.clone()
                                    };
                                    if let Some(session_id) = current_session_id {
                                        let _ = backend_event_tx
                                            .send(BackendEvent::CompactContext { session_id })
                                            .await;
                                    }
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
                                        let was_root_esc = matches!(
                                            key.code,
                                            crossterm::event::KeyCode::Esc
                                        ) && state.popup_stack.depth() == 1;
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
                                        )
                                    };
                                    let (
                                        committed,
                                        was_root_esc,
                                        still_active,
                                        depth_before,
                                        depth_after,
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
                                        clear_slash_command_word(&mut input_manager.textarea);
                                        if let Some(next_event) = handle_command_result(
                                            cmd,
                                            &app_state,
                                            &renderer,
                                            &backend_event_tx,
                                        )
                                        .await
                                        {
                                            // Forward any backend event the command produced.
                                            let _ = backend_event_tx.send(next_event).await;
                                        }
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
        command_executor_factory: code_assistant_core::backend::CommandExecutorFactory,
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

        // Create session manager
        let session_manager = SessionManager::new(
            session_persistence,
            session_config_template,
            config.model.clone(),
            code_assistant_core::tools::default_registry(),
        );
        let multi_session_manager = Arc::new(Mutex::new(session_manager));

        // Create terminal UI and wrap as UserInterface
        let terminal_ui = TerminalUI::new_with_state(app_state.clone());
        let ui: Arc<dyn UserInterface> = Arc::new(terminal_ui.clone());

        // Setup UI event channel for display fragments
        let (ui_event_tx, ui_event_rx) =
            async_channel::unbounded::<code_assistant_core::ui::UiEvent>();
        terminal_ui.set_event_sender(ui_event_tx);

        // Setup backend communication channels
        let (backend_event_tx, backend_event_rx) = async_channel::unbounded::<BackendEvent>();
        let (backend_response_tx, backend_response_rx) =
            async_channel::unbounded::<BackendResponse>();

        // Spawn backend handler
        let backend_task = {
            let multi_session_manager = multi_session_manager.clone();
            let runtime_options = Arc::new(BackendRuntimeOptions {
                record_path: config.record.clone(),
                playback_path: config.playback.clone(),
                fast_playback: config.fast_playback,
                command_executor_factory,
            });
            let ui = ui.clone();

            tokio::spawn(async move {
                handle_backend_events(
                    backend_event_rx,
                    backend_response_tx,
                    multi_session_manager,
                    runtime_options,
                    ui,
                )
                .await;
            })
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
                backend_event_tx
                    .send(BackendEvent::LoadSession {
                        session_id: existing_session_id.clone(),
                    })
                    .await?;
                session_id = Some(existing_session_id);
            } else {
                debug!("No previous session found");
            }
        }

        // Create new session if we don't have one yet
        if session_id.is_none() {
            debug!("Creating new session");

            backend_event_tx
                .send(BackendEvent::CreateNewSession {
                    name: None,
                    initial_project: None,
                })
                .await?;

            match backend_response_rx.recv().await? {
                BackendResponse::SessionCreated {
                    session_id: new_session_id,
                } => {
                    debug!("Created new session: {}", new_session_id);
                    backend_event_tx
                        .send(BackendEvent::LoadSession {
                            session_id: new_session_id.clone(),
                        })
                        .await?;
                    session_id = Some(new_session_id);
                }
                BackendResponse::Error { message } => {
                    return Err(anyhow::anyhow!("Failed to create session: {message}"));
                }
                _ => {
                    return Err(anyhow::anyhow!("Unexpected response when creating session"));
                }
            }
        }

        let session_id = session_id.expect("Session ID should be set at this point");

        debug!("Terminal TUI connected to session: {}", session_id);

        // Immediately set current_session_id so first Enter can send
        {
            let mut state = app_state.lock().await;
            state.current_session_id = Some(session_id.clone());
        }

        // Kick off a session list refresh (optional but useful)
        let _ = backend_event_tx.try_send(BackendEvent::ListSessions);

        // Spawn a background task to process UI events from display fragments
        {
            let terminal_ui_clone = terminal_ui.clone();
            tokio::spawn(async move {
                while let Ok(event) = ui_event_rx.recv().await {
                    let _ = terminal_ui_clone.send_event(event).await;
                }
            });
        }

        // Spawn a background task to translate backend responses into UiEvents
        {
            let ui_clone = ui.clone();
            let app_state_clone = app_state.clone();
            tokio::spawn(async move {
                while let Ok(resp) = backend_response_rx.recv().await {
                    match resp {
                        BackendResponse::SessionsListed { sessions } => {
                            let _ = ui_clone
                                .send_event(code_assistant_core::ui::UiEvent::UpdateChatList {
                                    sessions,
                                })
                                .await;
                        }
                        BackendResponse::PendingMessageUpdated {
                            session_id: _,
                            message,
                        } => {
                            let _ = ui_clone
                                .send_event(
                                    code_assistant_core::ui::UiEvent::UpdatePendingMessage {
                                        message,
                                    },
                                )
                                .await;
                        }
                        BackendResponse::PendingMessageForEdit {
                            session_id: _,
                            message: _,
                        } => {
                            // For now, just clear pending in UI
                            let _ = ui_clone
                                .send_event(
                                    code_assistant_core::ui::UiEvent::UpdatePendingMessage {
                                        message: None,
                                    },
                                )
                                .await;
                        }
                        BackendResponse::Error { message } => {
                            // Display error in status area
                            let _ = ui_clone
                                .send_event(code_assistant_core::ui::UiEvent::DisplayError {
                                    message,
                                })
                                .await;
                        }
                        BackendResponse::SessionCreated { .. } => {}
                        BackendResponse::SessionDeleted { .. } => {}
                        BackendResponse::ModelSwitched {
                            session_id: _,
                            model_name,
                            warning,
                            allowed_models: _,
                        } => {
                            // Update current model in app state
                            let mut state = app_state_clone.lock().await;
                            state.update_current_model(Some(model_name.clone()));
                            let info = match warning {
                                Some(w) => format!("Switched to model: {model_name} ({w})"),
                                None => format!("Switched to model: {model_name}"),
                            };
                            state.set_info_message(Some(info));
                        }

                        BackendResponse::SandboxPolicyChanged {
                            session_id: _,
                            policy,
                        } => {
                            let mut state = app_state_clone.lock().await;
                            state.update_sandbox_policy(Some(policy.clone()));
                            state.set_info_message(Some(format!(
                                "Sandbox mode set to {:?}",
                                policy
                            )));
                        }

                        BackendResponse::SubAgentCancelled {
                            session_id: _,
                            tool_id: _,
                        } => {
                            // Sub-agent cancellation handled; the sub-agent will
                            // update its tool output via the normal mechanism
                        }

                        BackendResponse::MessageEditReady { .. }
                        | BackendResponse::BranchSwitched { .. }
                        | BackendResponse::MessageEditCancelled { .. } => {
                            // Session branching not supported in terminal UI
                        }

                        BackendResponse::BranchesAndWorktreesListed { .. }
                        | BackendResponse::WorktreeSwitched { .. }
                        | BackendResponse::WorktreeCreated { .. } => {
                            // Worktree management not supported in terminal UI
                        }

                        BackendResponse::ProjectAdded { .. }
                        | BackendResponse::ProjectPersisted { .. }
                        | BackendResponse::ProjectAlreadyExists { .. } => {
                            // Project management not supported in terminal UI
                        }
                    }
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

        // Create redraw notification channel
        let (redraw_tx, redraw_rx) = tokio::sync::watch::channel::<()>(());
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
            let _ = backend_event_tx.try_send(BackendEvent::SendUserMessage {
                session_id: session_id.clone(),
                message: task.clone(),
                attachments: Vec::new(),
                branch_parent_id: None,
            });
        }

        // Start main event loop in a separate task
        let event_loop_handle = tokio::spawn(event_loop(
            input_manager,
            renderer.clone(),
            app_state,
            terminal_ui.cancel_flag.clone(),
            backend_event_tx,
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
