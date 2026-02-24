use crate::app::AgentRunConfig;
use crate::config;
use crate::persistence::FileSessionPersistence;
use crate::session::manager::SessionManager;
use crate::session::SessionConfig;
use crate::ui::backend::{
    handle_backend_events, BackendEvent, BackendResponse, BackendRuntimeOptions,
};
use crate::ui::terminal::{
    input::{InputManager, KeyEventResult},
    renderer::ProductionTerminalRenderer,
    state::AppState,
    tui,
    ui::TerminalUI,
};
use crate::ui::UserInterface;
use anyhow::Result;

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
                                            if matches!(
                                                activity_state,
                                                Some(crate::session::instance::SessionActivityState::Idle)
                                            ) {
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
                                            Some(crate::session::instance::SessionActivityState::Idle)
                                            | None => {
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
                            }
                            needs_redraw = true;
                        }
                        Event::Paste(pasted) => {
                            // Many terminals convert newlines to \r when pasting;
                            // normalize before processing.
                            let pasted = pasted.replace('\r', "\n");
                            input_manager.handle_paste(pasted);
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

pub struct TerminalTuiApp {}

impl TerminalTuiApp {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn run(&self, config: &AgentRunConfig) -> Result<()> {
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
        };

        // Create session manager
        let session_manager = SessionManager::new(
            session_persistence,
            session_config_template,
            config.model.clone(),
        );
        let multi_session_manager = Arc::new(Mutex::new(session_manager));

        // Create terminal UI and wrap as UserInterface
        let terminal_ui = TerminalUI::new_with_state(app_state.clone());
        let ui: Arc<dyn UserInterface> = Arc::new(terminal_ui.clone());

        // Setup UI event channel for display fragments
        let (ui_event_tx, ui_event_rx) = async_channel::unbounded::<crate::ui::UiEvent>();
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
                .send(BackendEvent::CreateNewSession { name: None })
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
                                .send_event(crate::ui::UiEvent::UpdateChatList { sessions })
                                .await;
                        }
                        BackendResponse::PendingMessageUpdated {
                            session_id: _,
                            message,
                        } => {
                            let _ = ui_clone
                                .send_event(crate::ui::UiEvent::UpdatePendingMessage { message })
                                .await;
                        }
                        BackendResponse::PendingMessageForEdit {
                            session_id: _,
                            message: _,
                        } => {
                            // For now, just clear pending in UI
                            let _ = ui_clone
                                .send_event(crate::ui::UiEvent::UpdatePendingMessage {
                                    message: None,
                                })
                                .await;
                        }
                        BackendResponse::Error { message } => {
                            // Display error in status area
                            let _ = ui_clone
                                .send_event(crate::ui::UiEvent::DisplayError { message })
                                .await;
                        }
                        BackendResponse::SessionCreated { .. } => {}
                        BackendResponse::SessionDeleted { .. } => {}
                        BackendResponse::ModelSwitched {
                            session_id: _,
                            model_name,
                        } => {
                            // Update current model in app state
                            let mut state = app_state_clone.lock().await;
                            state.update_current_model(Some(model_name.clone()));
                            state.set_info_message(Some(format!(
                                "Switched to model: {model_name}",
                            )));
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
