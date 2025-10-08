use super::AgentRunConfig;
use crate::config::DefaultProjectManager;
use crate::session::{SessionConfig, SessionManager};
use crate::ui::{self, UserInterface};
use crate::utils::DefaultCommandExecutor;
use anyhow::Result;
use llm::factory::{create_llm_client_from_model, LLMClientConfig, LLMProviderType};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info};

pub fn run(config: AgentRunConfig) -> Result<()> {
    // Create shared state between GUI and backend
    let gui = ui::gpui::Gpui::new();

    // Setup unified backend communication
    let (backend_event_rx, backend_response_tx) = gui.setup_backend_communication();

    // Setup dynamic types for MultiSessionManager
    let root_path = config.path.canonicalize()?;
    let persistence = crate::persistence::FileSessionPersistence::new();

    let session_config_template = SessionConfig {
        init_path: Some(root_path.clone()),
        initial_project: String::new(),
        tool_syntax: config.tool_syntax,
        use_diff_blocks: config.use_diff_format,
    };

    // Create the new SessionManager
    let multi_session_manager = Arc::new(Mutex::new(SessionManager::new(
        persistence,
        session_config_template,
    )));

    // Clone GUI before moving it into thread
    let gui_for_thread = gui.clone();
    let task_clone = config.task.clone();
    let model = config.model.clone();
    let record = config.record.clone();
    let playback = config.playback.clone();
    let fast_playback = config.fast_playback;

    // Start the simplified backend thread
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();

        runtime.block_on(async {
            if let Some(initial_task) = task_clone {
                // Task provided - create new session and start agent
                debug!("Creating initial session with task: {}", initial_task);

                // TODO: Replace with proper model selection in Phase 3
                let session_model_config =
                    model
                        .as_ref()
                        .map(|m| crate::persistence::SessionModelConfig {
                            model_name: m.clone(),
                            record_path: record.clone(),
                        });

                let session_id = {
                    let mut manager = multi_session_manager.lock().await;
                    manager
                        .create_session_with_config(None, None, session_model_config.clone())
                        .unwrap()
                };

                debug!("Created initial session: {}", session_id);

                // Connect session to UI and start agent
                let ui_events = {
                    let mut manager = multi_session_manager.lock().await;
                    manager
                        .set_active_session(session_id.clone())
                        .await
                        .unwrap_or_else(|e| {
                            error!("Failed to set active session: {}", e);
                            Vec::new()
                        })
                };

                for event in ui_events {
                    if let Err(e) = gui_for_thread.send_event(event).await {
                        error!("Failed to send UI event: {}", e);
                    }
                }

                let project_manager = Box::new(DefaultProjectManager::new());
                let command_executor = Box::new(DefaultCommandExecutor);
                let user_interface: Arc<dyn crate::ui::UserInterface> =
                    Arc::new(gui_for_thread.clone());

                let llm_client = if let Some(ref model_name) = model {
                    create_llm_client_from_model(model_name, playback.clone(), fast_playback)
                } else {
                    // Use default model if none specified
                    create_llm_client_from_model(
                        "Claude Sonnet 4.5",
                        playback.clone(),
                        fast_playback,
                    )
                }
                .await
                .expect("Failed to create LLM client");

                {
                    let mut manager = multi_session_manager.lock().await;
                    manager
                        .start_agent_for_message(
                            &session_id,
                            vec![llm::ContentBlock::new_text(initial_task)],
                            llm_client,
                            project_manager,
                            command_executor,
                            user_interface,
                        )
                        .await
                        .expect("Failed to start agent with initial task");
                }

                debug!("Started agent for initial session");
            } else {
                // No task - connect to latest existing session
                info!("No task provided, connecting to latest session");

                let latest_session_id = {
                    let manager = multi_session_manager.lock().await;
                    manager.get_latest_session_id().unwrap_or(None)
                };

                if let Some(session_id) = latest_session_id {
                    debug!("Connecting to existing session: {}", session_id);

                    let ui_events = {
                        let mut manager = multi_session_manager.lock().await;
                        manager
                            .set_active_session(session_id.clone())
                            .await
                            .unwrap_or_else(|e| {
                                error!("Failed to set active session: {}", e);
                                Vec::new()
                            })
                    };

                    for event in ui_events {
                        if let Err(e) = gui_for_thread.send_event(event).await {
                            error!("Failed to send UI event: {}", e);
                        }
                    }
                } else {
                    info!("No existing sessions found - creating a new session automatically");

                    // Create a new session automatically
                    let new_session_id = {
                        // TODO: Replace with proper model selection in Phase 3
                        let model_config =
                            model
                                .as_ref()
                                .map(|m| crate::persistence::SessionModelConfig {
                                    model_name: m.clone(),
                                    record_path: record.clone(),
                                });

                        let mut manager = multi_session_manager.lock().await;
                        manager
                            .create_session_with_config(None, None, model_config)
                            .unwrap_or_else(|e| {
                                error!("Failed to create new session: {}", e);
                                // Return a fallback session ID if creation fails
                                "fallback".to_string()
                            })
                    };

                    if new_session_id != "fallback" {
                        debug!("Created new session: {}", new_session_id);

                        // Connect to the newly created session
                        let ui_events = {
                            let mut manager = multi_session_manager.lock().await;
                            manager
                                .set_active_session(new_session_id.clone())
                                .await
                                .unwrap_or_else(|e| {
                                    error!("Failed to set active session: {}", e);
                                    Vec::new()
                                })
                        };

                        for event in ui_events {
                            if let Err(e) = gui_for_thread.send_event(event).await {
                                error!("Failed to send UI event: {}", e);
                            }
                        }
                    }
                }
            }

            // TODO: Replace with proper model-based configuration in Phase 4
            let cfg = Arc::new(LLMClientConfig {
                provider: LLMProviderType::Anthropic,
                model,
                base_url: None,
                aicore_config: None,
                num_ctx: 8192,
                record_path: record,
                playback_path: playback,
                fast_playback,
            });

            crate::ui::backend::handle_backend_events(
                backend_event_rx,
                backend_response_tx,
                multi_session_manager,
                cfg,
                Arc::new(gui_for_thread) as Arc<dyn crate::ui::UserInterface>,
            )
            .await;
        });
    });

    // Run the GUI in the main thread
    gui.run_app();

    Ok(())
}
