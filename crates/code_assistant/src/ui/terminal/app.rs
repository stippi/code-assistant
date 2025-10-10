use crate::app::AgentRunConfig;
use crate::persistence::FileSessionPersistence;
use crate::session::manager::SessionManager;
use crate::session::SessionConfig;
use crate::ui::backend::{handle_backend_events, BackendEvent, BackendResponse};
use crate::ui::terminal::{
    input::{InputManager, KeyEventResult},
    renderer::ProductionTerminalRenderer,
    state::AppState,
    ui::TerminalTuiUI,
};
use crate::ui::UserInterface;
use anyhow::Result;

use llm::factory::LLMClientConfig;
use ratatui::crossterm::event::{self, Event};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::debug;

/// Main event loop for handling terminal events
async fn event_loop(
    mut input_manager: InputManager,
    renderer: Arc<Mutex<ProductionTerminalRenderer>>,
    app_state: Arc<Mutex<AppState>>,
    backend_event_tx: async_channel::Sender<BackendEvent>,
) -> Result<()> {
    loop {
        // Sync state and render the UI
        {
            let mut renderer_guard = renderer.lock().await;
            let state = app_state.lock().await;

            // Sync info message from state to renderer
            if let Some(ref info_msg) = state.info_message {
                renderer_guard.set_info(info_msg.clone());
            } else {
                renderer_guard.clear_info();
            }

            drop(state); // Release the lock before rendering
            renderer_guard.render(&input_manager.textarea)?;
        }

        // Check for events with a timeout
        if event::poll(tokio::time::Duration::from_millis(8))? {
            match event::read()? {
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
                                // Check if agent is running and cancel it
                                let activity_state = {
                                    let state = app_state.lock().await;
                                    state.activity_state.clone()
                                };

                                if let Some(state) = activity_state {
                                    if !matches!(
                                        state,
                                        crate::session::instance::SessionActivityState::Idle
                                    ) {
                                        // Agent is running, send cancel request
                                        // This would need to be implemented similar to GPUI's cancel mechanism
                                        debug!("Escape pressed - would cancel running agent");
                                        // TODO: Implement agent cancellation for terminal UI
                                    }
                                }
                            }
                        }
                        KeyEventResult::SendMessage(message) => {
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
                                    | None => BackendEvent::SendUserMessage {
                                        session_id,
                                        message,
                                        attachments: Vec::new(),
                                    },
                                    _ => BackendEvent::QueueUserMessage {
                                        session_id,
                                        message,
                                        attachments: Vec::new(),
                                    },
                                };

                                let _ = backend_event_tx.send(event).await;
                            }
                        }
                        KeyEventResult::Continue => {
                            // Do nothing, just continue the loop
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
                    }
                }
                Event::Resize(_, _) => {
                    // Ratatui handles resize automatically, but we might need to update viewport
                    let mut renderer_guard = renderer.lock().await;
                    let input_height =
                        renderer_guard.calculate_input_height(&input_manager.textarea);
                    renderer_guard.update_size(input_height)?;
                }
                _ => {
                    // Ignore other events
                }
            }
        }
    }

    Ok(())
}

pub struct TerminalTuiApp {
    app_state: Arc<Mutex<AppState>>,
}

impl TerminalTuiApp {
    pub fn new() -> Self {
        Self {
            app_state: Arc::new(Mutex::new(AppState::new())),
        }
    }

    pub async fn run(&self, config: &AgentRunConfig) -> Result<()> {
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
        };

        // Create session manager
        let session_manager = SessionManager::new(session_persistence, session_config_template);
        let multi_session_manager = Arc::new(Mutex::new(session_manager));

        // Create terminal UI and wrap as UserInterface
        let terminal_ui = TerminalTuiUI::new();
        let ui: Arc<dyn UserInterface> = Arc::new(terminal_ui.clone());

        // Setup UI event channel for display fragments
        let (ui_event_tx, ui_event_rx) = async_channel::unbounded::<crate::ui::UiEvent>();
        terminal_ui.set_event_sender(ui_event_tx).await;

        // Setup backend communication channels
        let (backend_event_tx, backend_event_rx) = async_channel::unbounded::<BackendEvent>();
        let (backend_response_tx, backend_response_rx) =
            async_channel::unbounded::<BackendResponse>();

        // Get model name or use default
        let model_name = config
            .model
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Model name is required"))?;

        // Spawn backend handler
        let backend_task = {
            let multi_session_manager = multi_session_manager.clone();
            let model_name_for_backend = model_name.clone();
            let ui = ui.clone();
            let record_path = config.record.clone();
            let playback_path = config.playback.clone();
            let fast_playback = config.fast_playback;

            tokio::spawn(async move {
                // TODO: Replace with proper model-based backend in Phase 4
                let temp_llm_config = Arc::new(LLMClientConfig {
                    provider: llm::factory::LLMProviderType::Anthropic,
                    model: Some(model_name_for_backend),
                    base_url: None,
                    aicore_config: None,
                    num_ctx: 8192,
                    record_path,
                    playback_path,
                    fast_playback,
                });

                handle_backend_events(
                    backend_event_rx,
                    backend_response_tx,
                    multi_session_manager,
                    temp_llm_config,
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
                    return Err(anyhow::anyhow!("Failed to create session: {}", message));
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
            let mut state = self.app_state.lock().await;
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
            let app_state_clone = self.app_state.clone();
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
                    }
                }
            });
        }

        // Flush stdout to ensure instructions are displayed
        std::io::Write::flush(&mut std::io::stdout())?;

        // Initialize components
        let input_manager = InputManager::new();
        let mut renderer = ProductionTerminalRenderer::new()?;

        // Setup panic hook to ensure terminal is cleaned up on panic
        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            // Try to disable raw mode on panic
            let _ = ratatui::crossterm::terminal::disable_raw_mode();
            original_hook(panic_info);
        }));

        // Setup terminal AFTER printing instructions
        renderer.setup_terminal()?;

        let renderer = Arc::new(Mutex::new(renderer));

        // Bind renderer to UI for message printing and input redraws
        terminal_ui.set_renderer_async(renderer.clone()).await;

        // Create redraw notification channel
        let (redraw_tx, mut redraw_rx) = tokio::sync::watch::channel::<()>(());
        terminal_ui.set_redraw_sender(redraw_tx.clone());

        // Print welcome message to content area
        {
            let mut renderer_guard = renderer.lock().await;
            let log_file_path = dirs::cache_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("code-assistant")
                .join("terminal-ui.log");

            let welcome_text = format!(
                "Welcome to Code Assistant Terminal UI!\n\
                Type your message and press Enter to send.\n\
                Use Shift+Enter for multi-line input.\n\
                Press Ctrl+C to quit.\n\
                \n\
                Debug logs are written to: {}\n\n",
                log_file_path.display()
            );
            renderer_guard.add_instruction_message(&welcome_text)?;
        }

        // Send initial task if provided
        if let Some(task) = &config.task {
            let _ = backend_event_tx.try_send(BackendEvent::SendUserMessage {
                session_id: session_id.clone(),
                message: task.clone(),
                attachments: Vec::new(),
            });
        }

        // Start main event loop in a separate task
        let mut event_loop_handle = tokio::spawn(event_loop(
            input_manager,
            renderer.clone(),
            self.app_state.clone(),
            backend_event_tx,
        ));

        // Handle redraw notifications in main loop
        loop {
            tokio::select! {
                // Handle redraw notifications
                _ = redraw_rx.changed() => {
                    // Redraw is handled automatically by the renderer during the next render cycle
                }

                // Check if event loop finished (on Ctrl+C)
                result = &mut event_loop_handle => {
                    match result {
                        Ok(Ok(())) => break,
                        Ok(Err(e)) => return Err(e),
                        Err(e) => return Err(e.into()),
                    }
                }
            }
        }

        // Cleanup terminal
        {
            let mut renderer_guard = renderer.lock().await;
            renderer_guard.cleanup_terminal()?;
        }

        // Cancel the backend task
        backend_task.abort();

        println!("\nGoodbye!");
        Ok(())
    }
}
