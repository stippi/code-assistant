use crate::config::DefaultProjectManager;
use crate::session::{AgentConfig, SessionManager};
use crate::types::ToolSyntax;
use crate::ui::{self, UserInterface};
use crate::utils::DefaultCommandExecutor;
use anyhow::Result;
use llm::factory::{LLMClientConfig, LLMProviderType, create_llm_client};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info};

pub fn run(
    path: PathBuf,
    task: Option<String>,
    provider: LLMProviderType,
    model: Option<String>,
    base_url: Option<String>,
    aicore_config: Option<PathBuf>,
    num_ctx: usize,
    tool_syntax: ToolSyntax,
    use_diff_format: bool,
    record: Option<PathBuf>,
    playback: Option<PathBuf>,
    fast_playback: bool,
) -> Result<()> {
    // Create shared state between GUI and backend
    let gui = ui::gpui::Gpui::new();

    // Setup unified backend communication
    let (backend_event_rx, backend_response_tx) = gui.setup_backend_communication();

    // Setup dynamic types for MultiSessionManager
    let root_path = path.canonicalize()?;
    let persistence = crate::persistence::FileSessionPersistence::new();

    let agent_config = AgentConfig {
        tool_syntax,
        init_path: Some(root_path.clone()),
        initial_project: String::new(),
        use_diff_blocks: use_diff_format,
    };

    // Create the new SessionManager
    let multi_session_manager =
        Arc::new(Mutex::new(SessionManager::new(persistence, agent_config)));

    // Clone GUI before moving it into thread
    let gui_for_thread = gui.clone();
    let task_clone = task.clone();

    // Start the simplified backend thread
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();

        runtime.block_on(async {
            if let Some(initial_task) = task_clone {
                // Task provided - create new session and start agent
                debug!("Creating initial session with task: {}", initial_task);

                let session_id = {
                    let llm_config = crate::persistence::LlmSessionConfig {
                        provider: provider.clone(),
                        model: model.clone(),
                        base_url: base_url.clone(),
                        aicore_config: aicore_config.clone(),
                        num_ctx,
                        record_path: record.clone(),
                    };

                    let mut manager = multi_session_manager.lock().await;
                    manager.create_session_with_config(None, Some(llm_config)).unwrap()
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
                let user_interface: Arc<dyn crate::ui::UserInterface> = Arc::new(gui_for_thread.clone());

                let llm_client = create_llm_client(LLMClientConfig {
                    provider: provider.clone(),
                    model: model.clone(),
                    base_url: base_url.clone(),
                    aicore_config: aicore_config.clone(),
                    num_ctx,
                    record_path: record.clone(),
                    playback_path: playback.clone(),
                    fast_playback,
                })
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
                        let llm_config = crate::persistence::LlmSessionConfig {
                            provider: provider.clone(),
                            model: model.clone(),
                            base_url: base_url.clone(),
                            aicore_config: aicore_config.clone(),
                            num_ctx,
                            record_path: record.clone(),
                        };

                        let mut manager = multi_session_manager.lock().await;
                        manager.create_session_with_config(None, Some(llm_config)).unwrap_or_else(|e| {
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

            let cfg = Arc::new(LLMClientConfig {
                provider,
                model,
                base_url,
                aicore_config,
                num_ctx,
                record_path: record,
                playback_path: playback,
                fast_playback,
            });

            crate::ui::gpui::backend::handle_backend_events(
                backend_event_rx,
                backend_response_tx,
                multi_session_manager,
                cfg,
                gui_for_thread,
            )
            .await;
        });
    });

    // Run the GUI in the main thread
    gui.run_app();

    Ok(())
}
