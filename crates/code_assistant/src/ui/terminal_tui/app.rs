use crate::app::AgentRunConfig;
use crate::persistence::FileSessionPersistence;
use crate::session::manager::{AgentConfig, SessionManager};
use crate::ui::backend::{handle_backend_events, BackendEvent, BackendResponse};
use crate::ui::terminal_tui::{
    input::InputManager,
    renderer::TerminalRenderer,
    state::AppState,
    ui::TerminalTuiUI,
};
use crate::ui::UserInterface;
use anyhow::Result;
use ratatui::crossterm::event::{self, Event};
use llm::factory::LLMClientConfig;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::debug;

/// Main event loop for handling terminal events
async fn event_loop(
    mut input_manager: InputManager,
    renderer: Arc<Mutex<TerminalRenderer>>,
    app_state: Arc<Mutex<AppState>>,
    backend_event_tx: async_channel::Sender<BackendEvent>,
) -> Result<()> {
    loop {
        // Render the UI
        {
            let mut renderer_guard = renderer.lock().await;
            renderer_guard.render(&input_manager.textarea)?;
        }

        // Check for events with a timeout
        if event::poll(tokio::time::Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key_event) => {
                    let (should_quit, user_message) = input_manager.handle_key_event(key_event);

                    if should_quit {
                        break;
                    }

                    if let Some(message) = user_message {
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
                                Some(crate::session::instance::SessionActivityState::Idle) | None => {
                                    BackendEvent::SendUserMessage {
                                        session_id,
                                        message,
                                        attachments: Vec::new(),
                                    }
                                }
                                _ => BackendEvent::QueueUserMessage {
                                    session_id,
                                    message,
                                    attachments: Vec::new(),
                                },
                            };

                            let _ = backend_event_tx.send(event).await;
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
        let agent_config = AgentConfig {
            tool_syntax: config.tool_syntax,
            init_path: Some(root_path.clone()),
            initial_project: root_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("unknown")
                .to_string(),
            use_diff_blocks: config.use_diff_format,
        };

        // Create session manager
        let session_manager = SessionManager::new(session_persistence, agent_config);
        let multi_session_manager = Arc::new(Mutex::new(session_manager));

        // Create terminal UI and wrap as UserInterface
        let terminal_ui = TerminalTuiUI::new();
        let ui: Arc<dyn UserInterface> = Arc::new(terminal_ui.clone());

        // Setup backend communication channels
        let (backend_event_tx, backend_event_rx) = async_channel::unbounded::<BackendEvent>();
        let (backend_response_tx, backend_response_rx) =
            async_channel::unbounded::<BackendResponse>();

        // Create LLM client config
        let llm_config = Arc::new(LLMClientConfig {
            provider: config.provider.clone(),
            model: config.model.clone(),
            base_url: config.base_url.clone(),
            aicore_config: config.aicore_config.clone(),
            num_ctx: config.num_ctx,
            record_path: config.record.clone(),
            playback_path: config.playback.clone(),
            fast_playback: config.fast_playback,
        });

        // Spawn backend handler
        let backend_task = {
            let multi_session_manager = multi_session_manager.clone();
            let llm_config = llm_config.clone();
            let ui = ui.clone();

            tokio::spawn(async move {
                handle_backend_events(
                    backend_event_rx,
                    backend_response_tx,
                    multi_session_manager,
                    llm_config,
                    ui,
                )
                .await;
            })
        };

        // Determine which session to use and load it
        let session_id = if config.continue_task {
            let latest_session_id = {
                let manager = multi_session_manager.lock().await;
                manager.get_latest_session_id().unwrap_or(None)
            };

            match latest_session_id {
                Some(session_id) => {
                    debug!("Continuing from latest session: {}", session_id);
                    backend_event_tx
                        .send(BackendEvent::LoadSession {
                            session_id: session_id.clone(),
                        })
                        .await?;
                    session_id
                }
                None => {
                    debug!("No previous session found, creating new session");
                    backend_event_tx
                        .send(BackendEvent::CreateNewSession { name: None })
                        .await?;

                    match backend_response_rx.recv().await? {
                        BackendResponse::SessionCreated { session_id } => {
                            debug!("Created new session: {}", session_id);
                            backend_event_tx
                                .send(BackendEvent::LoadSession {
                                    session_id: session_id.clone(),
                                })
                                .await?;
                            session_id
                        }
                        BackendResponse::Error { message } => {
                            return Err(anyhow::anyhow!("Failed to create session: {}", message));
                        }
                        _ => {
                            return Err(anyhow::anyhow!(
                                "Unexpected response when creating session"
                            ));
                        }
                    }
                }
            }
        } else {
            debug!("Creating new session");
            backend_event_tx
                .send(BackendEvent::CreateNewSession { name: None })
                .await?;

            match backend_response_rx.recv().await? {
                BackendResponse::SessionCreated { session_id } => {
                    debug!("Created new session: {}", session_id);
                    backend_event_tx
                        .send(BackendEvent::LoadSession {
                            session_id: session_id.clone(),
                        })
                        .await?;
                    session_id
                }
                BackendResponse::Error { message } => {
                    return Err(anyhow::anyhow!("Failed to create session: {}", message));
                }
                _ => {
                    return Err(anyhow::anyhow!("Unexpected response when creating session"));
                }
            }
        };

        debug!("Terminal TUI connected to session: {}", session_id);

        // Immediately set current_session_id so first Enter can send
        {
            let mut state = self.app_state.lock().await;
            state.current_session_id = Some(session_id.clone());
        }

        // Kick off a session list refresh (optional but useful)
        let _ = backend_event_tx.try_send(BackendEvent::ListSessions);

        // Spawn a background task to translate backend responses into UiEvents
        {
            let ui_clone = ui.clone();
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
                            // Handle error via UI event
                            let _ = ui_clone
                                .send_event(crate::ui::UiEvent::AppendToTextBlock {
                                    content: format!("\n[error] {message}\n"),
                                })
                                .await;
                        }
                        BackendResponse::SessionCreated { .. } => {}
                        BackendResponse::SessionDeleted { .. } => {}
                    }
                }
            });
        }

        // Print initial instructions BEFORE entering raw mode
        println!("Code Assistant Terminal UI (Experimental)");
        println!("- Type in the input area at the bottom");
        println!("- Use arrow keys to move cursor, backspace/delete to edit");
        println!("- Press Enter to submit input, Shift+Enter for newline");
        println!("- Press Ctrl+C to quit");
        println!("- Background messages will appear above the input area");
        println!("- Supports Markdown formatting in messages");
        println!("---");

        // Flush stdout to ensure instructions are displayed
        std::io::Write::flush(&mut std::io::stdout())?;

        // Initialize components
        let input_manager = InputManager::new();
        let mut renderer = TerminalRenderer::new()?;

        // Setup terminal AFTER printing instructions
        renderer.setup_terminal()?;

        let renderer = Arc::new(Mutex::new(renderer));

        // Bind renderer to UI for message printing and input redraws
        terminal_ui.set_renderer_async(renderer.clone()).await;

        // Create redraw notification channel
        let (redraw_tx, mut redraw_rx) = tokio::sync::watch::channel::<()>(());
        terminal_ui.set_redraw_sender(redraw_tx.clone());

        // Print welcome message to content area using consistent API
        {
            let mut renderer_guard = renderer.lock().await;
            renderer_guard.start_live_block();
            renderer_guard.append_to_live_block("Welcome to Code Assistant Terminal UI!\n");
            renderer_guard.append_to_live_block("Type your message and press Enter to send.\n");
            renderer_guard.append_to_live_block("Use Shift+Enter for multi-line input.\n");
            renderer_guard.append_to_live_block("Press Ctrl+C to quit.\n\n");
            renderer_guard.finalize_live_block()?;
        }

        // Send initial task if provided
        if let Some(task) = &config.task {
            {
                let mut renderer_guard = renderer.lock().await;
                renderer_guard.start_live_block();
                renderer_guard.append_to_live_block(&format!("Starting with task: {task}\n\n"));
                renderer_guard.finalize_live_block()?;
            }
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
