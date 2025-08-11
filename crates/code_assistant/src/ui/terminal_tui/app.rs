use crate::app::AgentRunConfig;
use crate::persistence::{ChatMetadata, FileSessionPersistence};
use crate::session::instance::SessionActivityState;
use crate::session::manager::{AgentConfig, SessionManager};
use crate::ui::backend::{BackendEvent, BackendResponse, handle_backend_events};
use crate::ui::terminal_tui::{
    state::AppState,
    ui::TerminalTuiUI,
    components::{input::InputComponent, messages::MessagesComponent, sidebar::SidebarComponent},
};
use crate::ui::{ui_events::MessageData, UserInterface};
use anyhow::Result;
use llm::factory::LLMClientConfig;
use ratatui::{
    crossterm::{event::{KeyCode, KeyModifiers}, execute, terminal::{disable_raw_mode, enable_raw_mode}, event::{DisableMouseCapture, EnableMouseCapture}},
    layout::{Constraint, Direction, Layout},
    Frame,
};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;
use tracing::debug;

#[derive(Clone)]
struct StateSnapshot {
    messages: Vec<MessageData>,
    sessions: Vec<ChatMetadata>,
    current_session_id: Option<String>,
    session_activity_states: HashMap<String, SessionActivityState>,
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

        // Create terminal UI
        let terminal_ui = TerminalTuiUI::new();
        let ui: Arc<dyn UserInterface> = Arc::new(terminal_ui);

        // Setup backend communication channels
        let (backend_event_tx, backend_event_rx) = async_channel::unbounded::<BackendEvent>();
        let (backend_response_tx, backend_response_rx) = async_channel::unbounded::<BackendResponse>();

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
            // Try to get the latest session
            let latest_session_id = {
                let manager = multi_session_manager.lock().await;
                manager.get_latest_session_id().unwrap_or(None)
            };

            match latest_session_id {
                Some(session_id) => {
                    debug!("Continuing from latest session: {}", session_id);
                    // Load the existing session
                    backend_event_tx
                        .send(BackendEvent::LoadSession {
                            session_id: session_id.clone(),
                        })
                        .await?;
                    session_id
                }
                None => {
                    debug!("No previous session found, creating new session");
                    // Create new session
                    backend_event_tx
                        .send(BackendEvent::CreateNewSession { name: None })
                        .await?;

                    // Wait for session creation response
                    match backend_response_rx.recv().await? {
                        BackendResponse::SessionCreated { session_id } => {
                            debug!("Created new session: {}", session_id);
                            // Load the new session
                            backend_event_tx
                                .send(BackendEvent::LoadSession { session_id: session_id.clone() })
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
                }
            }
        } else {
            debug!("Creating new session");
            // Create new session
            backend_event_tx
                .send(BackendEvent::CreateNewSession { name: None })
                .await?;

            // Wait for session creation response
            match backend_response_rx.recv().await? {
                BackendResponse::SessionCreated { session_id } => {
                    debug!("Created new session: {}", session_id);
                    // Load the new session
                    backend_event_tx
                        .send(BackendEvent::LoadSession { session_id: session_id.clone() })
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

        // Initialize terminal with inline viewport for natural scrolling
        enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        execute!(
            stdout,
            EnableMouseCapture
        )?;
        let backend = ratatui::backend::CrosstermBackend::new(stdout);
        let mut terminal = ratatui::Terminal::with_options(
            backend,
            ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Inline(3), // 3 lines for input area
            },
        )?;

        // Set up the UI components
        let _messages_component = crate::ui::terminal_tui::components::messages::MessagesComponent::new();
        let mut input_component = crate::ui::terminal_tui::components::input::InputComponent::new();
        let mut sidebar_component = crate::ui::terminal_tui::components::sidebar::SidebarComponent::new();

        // Create redraw notification channel
        let (redraw_tx, mut redraw_rx) = tokio::sync::watch::channel::<()>(());

        // Update the UI to use the redraw channel
        {
            let terminal_ui = ui.clone();
            if let Some(terminal_tui_ui) = terminal_ui.as_any().downcast_ref::<TerminalTuiUI>() {
                terminal_tui_ui.set_redraw_sender(redraw_tx.clone());
            }
        }

        // Main event loop
        let mut should_quit = false;
        let mut last_tick = tokio::time::Instant::now();
        let tick_rate = tokio::time::Duration::from_millis(50); // 20 FPS

        while !should_quit {
            // Handle input events
            if ratatui::crossterm::event::poll(std::time::Duration::from_millis(0))? {
                match ratatui::crossterm::event::read()? {
                    ratatui::crossterm::event::Event::Key(key) => {
                        match key.code {
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                should_quit = true;
                            }
                            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                sidebar_component.toggle_visibility();
                            }
                            _ => {
                                if sidebar_component.is_visible() {
                                    // Handle sidebar navigation
                                    match key.code {
                                        KeyCode::Up => {
                                            let sessions_len = {
                                                let state = self.app_state.lock().await;
                                                state.sessions.len()
                                            };
                                            sidebar_component.previous(sessions_len);
                                        }
                                        KeyCode::Down => {
                                            let sessions_len = {
                                                let state = self.app_state.lock().await;
                                                state.sessions.len()
                                            };
                                            sidebar_component.next(sessions_len);
                                        }
                                        KeyCode::Enter => {
                                            let (_sessions, selected_session_id) = {
                                                let state = self.app_state.lock().await;
                                                (state.sessions.clone(), sidebar_component.get_selected_session_id(&state.sessions))
                                            };
                                            if let Some(session_id) = selected_session_id {
                                                // Load the selected session
                                                backend_event_tx.send(BackendEvent::LoadSession { session_id }).await?;
                                                sidebar_component.toggle_visibility();
                                            }
                                        }
                                        KeyCode::Esc => {
                                            sidebar_component.toggle_visibility();
                                        }
                                        _ => {}
                                    }
                                } else {
                                    // Handle input component events
                                    use crate::ui::terminal_tui::components::input::InputResult;
                                    match input_component.handle_input(ratatui::crossterm::event::Event::Key(key)) {
                                        InputResult::SendMessage(content) => {
                                            if !content.trim().is_empty() {
                                                let current_session_id = {
                                                    let state = self.app_state.lock().await;
                                                    state.current_session_id.clone()
                                                };
                                                if let Some(session_id) = current_session_id {
                                                    // Check if we should send or queue based on activity state
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
                                                        _ => {
                                                            BackendEvent::QueueUserMessage {
                                                                session_id,
                                                                message: content,
                                                                attachments: Vec::new(),
                                                            }
                                                        }
                                                    };
                                                    backend_event_tx.send(event).await?;
                                                }
                                            }
                                        }
                                        InputResult::Cancel => {
                                            // Cancel current operation
                                            let current_session_id = {
                                                let state = self.app_state.lock().await;
                                                state.current_session_id.clone()
                                            };
                                            if let Some(_session_id) = current_session_id {
                                                if let Some(terminal_tui_ui) = ui.as_any().downcast_ref::<TerminalTuiUI>() {
                                                    // Set cancel flag
                                                    if let Ok(mut cancel_flag) = terminal_tui_ui.cancel_flag.try_lock() {
                                                        *cancel_flag = true;
                                                    }
                                                }
                                            }
                                        }
                                        InputResult::None => {}
                                    }
                                }
                            }
                        }
                    }
                    ratatui::crossterm::event::Event::Resize(_, _) => {
                        // Terminal was resized, will be handled in render
                    }
                    _ => {}
                }
            }

            // Check for redraw notifications
            if redraw_rx.has_changed().unwrap_or(false) {
                // Redraw requested
                let _ = redraw_rx.borrow_and_update(); // Mark as seen
            }

            // Render only the input area in the inline viewport
            if last_tick.elapsed() >= tick_rate {
                terminal.draw(|frame| {
                    // The entire frame area is our input area (3 lines as specified in viewport)
                    input_component.render(frame, frame.area());
                })?;
                last_tick = tokio::time::Instant::now();
            }

            // Small sleep to prevent busy waiting
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }

        // Cleanup terminal
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        debug!("Terminal TUI shutting down");

        // Cancel the backend task
        backend_task.abort();

        Ok(())
    }

    fn render(
        &self,
        frame: &mut Frame,
        messages_component: &mut MessagesComponent,
        input_component: &mut InputComponent,
        sidebar_component: &mut SidebarComponent,
        state: &StateSnapshot,
    ) {
        if sidebar_component.is_visible() {
            // Split screen for sidebar
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
                .split(frame.area());

            // Render sidebar
            sidebar_component.render(
                frame,
                chunks[0],
                &state.sessions,
                state.current_session_id.as_deref(),
                &state.session_activity_states,
            );

            // Render main area
            let main_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(3)])
                .split(chunks[1]);

            messages_component.render(frame, main_chunks[0], &state.messages);
            input_component.render(frame, main_chunks[1]);
        } else {
            // Full screen layout
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(3)])
                .split(frame.area());

            messages_component.render(frame, chunks[0], &state.messages);
            input_component.render(frame, chunks[1]);
        }
    }
}
