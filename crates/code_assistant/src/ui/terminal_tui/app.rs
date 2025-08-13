use crate::app::AgentRunConfig;
use crate::persistence::FileSessionPersistence;
use crate::session::instance::SessionActivityState;
use crate::session::manager::{AgentConfig, SessionManager};
use crate::ui::backend::{handle_backend_events, BackendEvent, BackendResponse};
use crate::ui::terminal_tui::{
    input_area::InputArea, renderer::TerminalRenderer, state::AppState, ui::TerminalTuiUI,
};
use crate::ui::UserInterface;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use llm::factory::LLMClientConfig;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::debug;

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
        let ui: Arc<dyn UserInterface> = Arc::new(terminal_ui);

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
                            // Print an inline error line minimally; do not break the UI
                            if let Some(tui_err) = ui_clone.as_any().downcast_ref::<TerminalTuiUI>()
                            {
                                if let Some(renderer) = tui_err.renderer.lock().await.as_ref() {
                                    let _ = renderer
                                        .append_content_chunk(&format!("\n[error] {message}\n"));
                                }
                            }
                        }
                        BackendResponse::SessionCreated { .. } => {}
                        BackendResponse::SessionDeleted { .. } => {}
                    }
                }
            });
        }

        // Initialize terminal renderer with scroll region and raw mode
        let renderer = TerminalRenderer::new()?; // input_height initialized to 1
                                                 // Bind renderer to UI for message printing and input redraws
        if let Some(tui) = ui.as_any().downcast_ref::<TerminalTuiUI>() {
            tui.set_renderer_async(renderer.clone()).await;
        }

        // Initialize scroll region to current input height before printing any content
        let (terminal_width, _) = crossterm::terminal::size()?;
        let input_area = InputArea::new(terminal_width);
        let _ = renderer.set_input_height(input_area.get_display_height());

        // Print welcome message to content area using consistent API
        let _ = renderer.append_content_chunk("Code Assistant Terminal UI (Experimental)\n");
        let _ = renderer.append_content_chunk("Type your message and press Enter to send.\n");
        let _ = renderer.append_content_chunk("Use Shift+Enter for multi-line input.\n");
        let _ = renderer.append_content_chunk("Press Ctrl+C to quit.\n\n");

        // Create redraw notification channel
        let (redraw_tx, mut redraw_rx) = tokio::sync::watch::channel::<()>(());
        if let Some(terminal_tui_ui) = ui.as_any().downcast_ref::<TerminalTuiUI>() {
            terminal_tui_ui.set_redraw_sender(redraw_tx.clone());
        }

        // Main event loop
        let mut should_quit = false;
        let mut last_tick = tokio::time::Instant::now();
        let tick_rate = tokio::time::Duration::from_millis(100); // 10 FPS

        // Initialize input area
        let (terminal_width, _) = crossterm::terminal::size()?;
        let mut input_area = InputArea::new(terminal_width);
        let mut spinner_idx: usize = 0;
        const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

        // Helper to compute prompt with optional spinner/status
        let make_prompt = |state: &AppState, tick: usize| -> String {
            match state.activity_state {
                Some(SessionActivityState::WaitingForResponse) => {
                    format!("{} ", SPINNER[tick % SPINNER.len()])
                }
                Some(SessionActivityState::RateLimited { seconds_remaining }) => {
                    format!("{} {}s ", SPINNER[tick % SPINNER.len()], seconds_remaining)
                }
                _ => "> ".to_string(),
            }
        };

        // Set initial scroll region and draw input
        let _ = renderer.set_input_height(input_area.get_display_height());
        let prompt = {
            let state = self.app_state.lock().await;
            make_prompt(&state, spinner_idx)
        };
        input_area.set_prompt_width(unicode_width::UnicodeWidthStr::width(prompt.as_str()));
        let _ = renderer.redraw_input(&prompt, &input_area);

        // Send initial task if provided
        if let Some(task) = &config.task {
            let _ = renderer.append_content_chunk(&format!("Starting with task: {task}\n\n"));
            let _ = backend_event_tx.try_send(BackendEvent::SendUserMessage {
                session_id: session_id.clone(),
                message: task.clone(),
                attachments: Vec::new(),
            });
        }

        while !should_quit {
            // Handle input events
            if event::poll(std::time::Duration::from_millis(10))? {
                match event::read()? {
                    Event::Key(key) => {
                        match key.code {
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                should_quit = true;
                            }
                            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                                input_area.insert_newline();
                                // Update input height and redraw
                                let _ = renderer.set_input_height(input_area.get_display_height());
                                let prompt = {
                                    let state = self.app_state.lock().await;
                                    make_prompt(&state, spinner_idx)
                                };
                                input_area.set_prompt_width(unicode_width::UnicodeWidthStr::width(
                                    prompt.as_str(),
                                ));
                                let _ = renderer.redraw_input(&prompt, &input_area);
                            }
                            KeyCode::Enter => {
                                let content = input_area.content().to_string();
                                if !content.trim().is_empty() {
                                    let current_session_id = {
                                        let state = self.app_state.lock().await;
                                        state.current_session_id.clone()
                                    };
                                    if let Some(session_id) = current_session_id {
                                        let activity_state = {
                                            let state = self.app_state.lock().await;
                                            state.activity_state.clone()
                                        };
                                        let event = match activity_state {
                                            Some(SessionActivityState::Idle) | None => {
                                                BackendEvent::SendUserMessage {
                                                    session_id,
                                                    message: content,
                                                    attachments: Vec::new(),
                                                }
                                            }
                                            _ => BackendEvent::QueueUserMessage {
                                                session_id,
                                                message: content,
                                                attachments: Vec::new(),
                                            },
                                        };
                                        let _ = backend_event_tx.try_send(event);
                                    }
                                }
                                input_area.clear();
                                // Update input height and redraw
                                let _ = renderer.set_input_height(input_area.get_display_height());
                                let prompt = {
                                    let state = self.app_state.lock().await;
                                    make_prompt(&state, spinner_idx)
                                };
                                input_area.set_prompt_width(unicode_width::UnicodeWidthStr::width(
                                    prompt.as_str(),
                                ));
                                let _ = renderer.redraw_input(&prompt, &input_area);
                            }
                            KeyCode::Backspace => {
                                input_area.backspace();
                                // Update input height and redraw
                                let _ = renderer.set_input_height(input_area.get_display_height());
                                let prompt = {
                                    let state = self.app_state.lock().await;
                                    make_prompt(&state, spinner_idx)
                                };
                                input_area.set_prompt_width(unicode_width::UnicodeWidthStr::width(
                                    prompt.as_str(),
                                ));
                                let _ = renderer.redraw_input(&prompt, &input_area);
                            }
                            KeyCode::Delete => {
                                input_area.delete_char();
                                // Update input height and redraw
                                let _ = renderer.set_input_height(input_area.get_display_height());
                                let prompt = {
                                    let state = self.app_state.lock().await;
                                    make_prompt(&state, spinner_idx)
                                };
                                input_area.set_prompt_width(unicode_width::UnicodeWidthStr::width(
                                    prompt.as_str(),
                                ));
                                let _ = renderer.redraw_input(&prompt, &input_area);
                            }
                            KeyCode::Left => {
                                input_area.move_cursor_left();
                                let prompt = {
                                    let state = self.app_state.lock().await;
                                    make_prompt(&state, spinner_idx)
                                };
                                input_area.set_prompt_width(unicode_width::UnicodeWidthStr::width(
                                    prompt.as_str(),
                                ));
                                let _ = renderer.redraw_input(&prompt, &input_area);
                            }
                            KeyCode::Right => {
                                input_area.move_cursor_right();
                                let prompt = {
                                    let state = self.app_state.lock().await;
                                    make_prompt(&state, spinner_idx)
                                };
                                input_area.set_prompt_width(unicode_width::UnicodeWidthStr::width(
                                    prompt.as_str(),
                                ));
                                let _ = renderer.redraw_input(&prompt, &input_area);
                            }
                            KeyCode::Up => {
                                input_area.move_cursor_up();
                                let prompt = {
                                    let state = self.app_state.lock().await;
                                    make_prompt(&state, spinner_idx)
                                };
                                input_area.set_prompt_width(unicode_width::UnicodeWidthStr::width(
                                    prompt.as_str(),
                                ));
                                let _ = renderer.redraw_input(&prompt, &input_area);
                            }
                            KeyCode::Down => {
                                input_area.move_cursor_down();
                                let prompt = {
                                    let state = self.app_state.lock().await;
                                    make_prompt(&state, spinner_idx)
                                };
                                input_area.set_prompt_width(unicode_width::UnicodeWidthStr::width(
                                    prompt.as_str(),
                                ));
                                let _ = renderer.redraw_input(&prompt, &input_area);
                            }
                            KeyCode::Home => {
                                input_area.move_cursor_to_start();
                                let prompt = {
                                    let state = self.app_state.lock().await;
                                    make_prompt(&state, spinner_idx)
                                };
                                input_area.set_prompt_width(unicode_width::UnicodeWidthStr::width(
                                    prompt.as_str(),
                                ));
                                let _ = renderer.redraw_input(&prompt, &input_area);
                            }
                            KeyCode::End => {
                                input_area.move_cursor_to_end();
                                let prompt = {
                                    let state = self.app_state.lock().await;
                                    make_prompt(&state, spinner_idx)
                                };
                                input_area.set_prompt_width(unicode_width::UnicodeWidthStr::width(
                                    prompt.as_str(),
                                ));
                                let _ = renderer.redraw_input(&prompt, &input_area);
                            }
                            KeyCode::Char(ch) => {
                                input_area.insert_char(ch);
                                // Update input height and redraw
                                let _ = renderer.set_input_height(input_area.get_display_height());
                                let prompt = {
                                    let state = self.app_state.lock().await;
                                    make_prompt(&state, spinner_idx)
                                };
                                input_area.set_prompt_width(unicode_width::UnicodeWidthStr::width(
                                    prompt.as_str(),
                                ));
                                let _ = renderer.redraw_input(&prompt, &input_area);
                            }
                            _ => {}
                        }
                    }
                    Event::Resize(cols, rows) => {
                        input_area.update_terminal_width(cols);
                        let _ = renderer.handle_resize(cols, rows);
                        let prompt = {
                            let state = self.app_state.lock().await;
                            make_prompt(&state, spinner_idx)
                        };
                        let _ = renderer.redraw_input(&prompt, &input_area);
                    }
                    _ => {}
                }
            }

            // Check for redraw notifications (immediate redraw to keep cursor stable)
            if redraw_rx.has_changed().unwrap_or(false) {
                let _ = redraw_rx.borrow_and_update();
                let prompt = {
                    let state = self.app_state.lock().await;
                    make_prompt(&state, spinner_idx)
                };
                let _ = renderer.redraw_input(&prompt, &input_area);
            }

            // Periodically redraw the input to reflect cursor, buffer, and spinner/status
            if last_tick.elapsed() >= tick_rate {
                spinner_idx = spinner_idx.wrapping_add(1);
                let prompt = {
                    let state = self.app_state.lock().await;
                    make_prompt(&state, spinner_idx)
                };
                let _ = renderer.redraw_input(&prompt, &input_area);
                last_tick = tokio::time::Instant::now();
            }

            // Small sleep to prevent busy waiting
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }

        // Cleanup terminal
        let _ = renderer.teardown();

        debug!("Terminal TUI shutting down");

        // Cancel the backend task
        backend_task.abort();

        Ok(())
    }
}
