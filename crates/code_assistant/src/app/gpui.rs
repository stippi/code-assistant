use super::AgentRunConfig;
use crate::config::DefaultProjectManager;
use crate::session::watcher::SessionWatcher;
use crate::session::{SessionConfig, SessionManager};
use crate::ui::gpui::terminal_executor::GpuiTerminalCommandExecutor;
use crate::ui::{self, UserInterface};
use anyhow::Result;
use llm::factory::create_llm_client_from_model;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

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

    let default_model = config.model.clone();
    let base_session_model_config =
        crate::persistence::SessionModelConfig::new(default_model.clone());

    // Clone persistence before it is moved into SessionManager so the
    // filesystem watcher can use it to resolve the sessions directory.
    let persistence_for_watcher = persistence.clone();

    // Create the new SessionManager
    let multi_session_manager = Arc::new(Mutex::new(SessionManager::new(
        persistence,
        session_config_template,
        default_model.clone(),
    )));

    // Clone GUI before moving it into thread
    let gui_for_thread = gui.clone();
    let task_clone = config.task.clone();
    let model = default_model;
    let base_model_config = base_session_model_config.clone();
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

                let session_id = {
                    let mut manager = multi_session_manager.lock().await;
                    manager
                        .create_session_with_config(None, None, Some(base_model_config.clone()))
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
                let command_executor =
                    Box::new(GpuiTerminalCommandExecutor::new(session_id.clone()));
                let user_interface: Arc<dyn crate::ui::UserInterface> =
                    Arc::new(gui_for_thread.clone());

                let llm_client = create_llm_client_from_model(
                    &model,
                    playback.clone(),
                    fast_playback,
                    record.clone(),
                )
                .await
                .expect("Failed to create LLM client");

                {
                    let mut manager = multi_session_manager.lock().await;
                    manager
                        .start_agent_for_message(
                            &session_id,
                            vec![llm::ContentBlock::new_text(initial_task)],
                            None, // Initial task is not a branch
                            llm_client,
                            project_manager,
                            command_executor,
                            user_interface,
                            None,
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
                        let mut manager = multi_session_manager.lock().await;
                        manager
                            .create_session_with_config(None, None, Some(base_model_config.clone()))
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

            // Start the filesystem watcher for cross-instance awareness.
            // The watcher runs in the background and emits UI events when
            // other code-assistant instances modify session files.

            let _session_watcher = match SessionWatcher::start(
                &persistence_for_watcher,
                gui_for_thread.event_sender(),
                gui_for_thread.current_session_id_ref(),
            ) {
                Ok(watcher) => {
                    info!("Filesystem session watcher started");
                    Some(watcher)
                }
                Err(e) => {
                    warn!("Failed to start filesystem session watcher: {e}");
                    None
                }
            };

            crate::ui::backend::handle_backend_events(
                backend_event_rx,
                backend_response_tx,
                multi_session_manager,
                Arc::new(crate::ui::backend::BackendRuntimeOptions {
                    record_path: record.clone(),
                    playback_path: playback.clone(),
                    fast_playback,
                }),
                Arc::new(gui_for_thread) as Arc<dyn crate::ui::UserInterface>,
            )
            .await;
        });
    });

    // Run the GUI in the main thread
    gui.run_app();

    Ok(())
}
