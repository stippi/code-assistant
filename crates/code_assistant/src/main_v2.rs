/// New implementation using MultiSessionManager architecture
fn run_agent_gpui_v2(
    path: PathBuf,
    task: Option<String>,
    session_manager: SessionManager, // Old single session manager (keep for compatibility)
    session_state: Option<crate::session::SessionState>,
    provider: LLMProviderType,
    model: Option<String>,
    base_url: Option<String>,
    num_ctx: usize,
    tools_type: ToolMode,
    record: Option<PathBuf>,
    playback: Option<PathBuf>,
    fast_playback: bool,
) -> Result<()> {
    use crate::session::{MultiSessionManager, AgentConfig};

    // Create shared state between GUI and backend
    let gui = ui::gpui::Gpui::new();

    // Setup new communication channels for the new architecture
    let (user_message_tx, user_message_rx) = async_channel::unbounded::<(String, String)>(); // (message, session_id)
    let (session_event_tx, session_event_rx) = async_channel::unbounded::<ui::gpui::ChatManagementEvent>();
    let (session_response_tx, session_response_rx) = async_channel::unbounded::<ui::gpui::ChatManagementResponse>();

    // Setup dynamic types for MultiSessionManager
    let root_path = path.canonicalize()?;
    let persistence = crate::persistence::FileStatePersistence::new(root_path.clone());

    let agent_config = AgentConfig {
        tool_mode: tools_type,
        init_path: Some(root_path.clone()),
        initial_project: None,
    };

    // Create the new MultiSessionManager
    let multi_session_manager = Arc::new(Mutex::new(MultiSessionManager::new(persistence, agent_config)));

    // Setup GUI communication (modified for new architecture)
    gui.setup_v2_communication(user_message_tx, session_event_tx, session_response_rx);

    // Start the backend thread with new architecture
    let multi_session_manager_clone = multi_session_manager.clone();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();

        runtime.block_on(async {
            // Handle session management events
            let session_event_task = {
                let multi_session_manager = multi_session_manager_clone.clone();
                let session_response_tx = session_response_tx.clone();

                tokio::spawn(async move {
                    while let Ok(event) = session_event_rx.recv().await {
                        tracing::info!("Session management event: {:?}", event);

                        let response = {
                            let manager = multi_session_manager.lock().unwrap();
                            match event {
                                ui::gpui::ChatManagementEvent::ListSessions => {
                                    match manager.list_all_sessions() {
                                        Ok(sessions) => ui::gpui::ChatManagementResponse::SessionsListed { sessions },
                                        Err(e) => ui::gpui::ChatManagementResponse::Error { message: e.to_string() },
                                    }
                                }
                                ui::gpui::ChatManagementEvent::CreateNewSession { name } => {
                                    drop(manager); // Release lock for async operation
                                    let mut manager = multi_session_manager.lock().unwrap();
                                    match manager.create_session(name).await {
                                        Ok(session_id) => {
                                            let display_name = format!("Chat {}", &session_id[5..13]);
                                            ui::gpui::ChatManagementResponse::SessionCreated { session_id, name: display_name }
                                        }
                                        Err(e) => ui::gpui::ChatManagementResponse::Error { message: e.to_string() },
                                    }
                                }
                                ui::gpui::ChatManagementEvent::LoadSession { session_id } => {
                                    drop(manager); // Release lock for async operation
                                    let mut manager = multi_session_manager.lock().unwrap();
                                    match manager.load_session(&session_id).await {
                                        Ok(messages) => {
                                            let _ = manager.set_active_session(session_id.clone()).await;
                                            ui::gpui::ChatManagementResponse::SessionLoaded { session_id, messages }
                                        }
                                        Err(e) => ui::gpui::ChatManagementResponse::Error { message: e.to_string() },
                                    }
                                }
                                ui::gpui::ChatManagementEvent::DeleteSession { session_id } => {
                                    drop(manager); // Release lock for async operation
                                    let mut manager = multi_session_manager.lock().unwrap();
                                    match manager.delete_session(&session_id).await {
                                        Ok(_) => ui::gpui::ChatManagementResponse::SessionDeleted { session_id },
                                        Err(e) => ui::gpui::ChatManagementResponse::Error { message: e.to_string() },
                                    }
                                }
                            }
                        };

                        let _ = session_response_tx.send(response).await;
                    }
                })
            };

            // Handle user messages (start agents on demand)
            let user_message_task = {
                let multi_session_manager = multi_session_manager_clone.clone();

                tokio::spawn(async move {
                    while let Ok((message, session_id)) = user_message_rx.recv().await {
                        tracing::info!("User message for session {}: {}", session_id, message);

                        // Create components for the agent
                        let project_manager = Box::new(DefaultProjectManager::new());
                        let command_executor = Box::new(DefaultCommandExecutor);
                        let user_interface: Box<dyn UserInterface> = Box::new(gui.clone());

                        // Create LLM client
                        let llm_client = match create_llm_client(
                            provider,
                            model.clone(),
                            base_url.clone(),
                            num_ctx,
                            record.clone(),
                            playback.clone(),
                            fast_playback,
                        ).await {
                            Ok(client) => client,
                            Err(e) => {
                                tracing::error!("Failed to create LLM client: {}", e);
                                continue;
                            }
                        };

                        // Start agent for this message
                        let mut manager = multi_session_manager.lock().unwrap();
                        if let Err(e) = manager.start_agent_for_message(
                            &session_id,
                            message,
                            llm_client,
                            project_manager,
                            command_executor,
                            Arc::new(user_interface),
                        ).await {
                            tracing::error!("Failed to start agent for session {}: {}", session_id, e);
                        }
                        drop(manager);
                    }
                })
            };

            // Monitor agent completions
            let completion_monitor_task = {
                let multi_session_manager = multi_session_manager_clone.clone();

                tokio::spawn(async move {
                    let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));

                    loop {
                        interval.tick().await;

                        let mut manager = multi_session_manager.lock().unwrap();
                        if let Ok(completed_sessions) = manager.check_agent_completions().await {
                            for session_id in completed_sessions {
                                tracing::info!("Agent completed for session: {}", session_id);
                                // Could send notifications to UI here
                            }
                        }
                        drop(manager);
                    }
                })
            };

            // Handle initial task or session state if provided
            if let Some(session_state) = session_state {
                // Load existing session into the new architecture
                tracing::info!("Loading existing session state into MultiSessionManager");
                // TODO: Could implement migration from old session to new architecture
            } else if let Some(task_str) = task {
                // Create initial session with task
                let mut manager = multi_session_manager.lock().unwrap();
                match manager.create_session(Some("Initial Task".to_string())).await {
                    Ok(session_id) => {
                        // Send initial task as user message to trigger agent
                        let _ = user_message_tx.send((task_str, session_id.clone())).await;

                        // Set as active session
                        let _ = manager.set_active_session(session_id).await;
                    }
                    Err(e) => {
                        tracing::error!("Failed to create initial session: {}", e);
                    }
                }
            }

            // Keep all tasks running
            let _ = tokio::try_join!(
                session_event_task,
                user_message_task,
                completion_monitor_task
            );
        });
    });

    // Run the GUI in the main thread
    gui.run_app();

    Ok(())
}
