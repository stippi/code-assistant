use super::AgentRunConfig;
use crate::session::watcher::SessionWatcher;
use crate::session::{SessionConfig, SessionManager};
use crate::ui::UserInterface;
use anyhow::Result;
use code_assistant_core::session::service::{AgentRuntimeOptions, SessionService};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

pub fn run(config: AgentRunConfig) -> Result<()> {
    // Create shared state between GUI and backend
    let gui = ui_gpui::Gpui::new();

    // Setup dynamic types for MultiSessionManager
    let persistence = crate::persistence::FileSessionPersistence::new();

    // In GPUI mode, don't use the current directory as default session path.
    // Sessions are project-based and get their path from the sidebar/projects.json.
    let session_config_template = SessionConfig {
        init_path: None,
        initial_project: String::new(),
        tool_syntax: config.tool_syntax,
        use_diff_blocks: config.use_diff_format,
        sandbox_policy: config.sandbox_policy.clone(),
        ..SessionConfig::default()
    };

    // Clone persistence before it is moved into SessionManager so the
    // filesystem watcher can use it to resolve the sessions directory.
    let persistence_for_watcher = persistence.clone();

    let multi_session_manager = Arc::new(Mutex::new(SessionManager::new(
        persistence,
        session_config_template,
        config.model.clone(),
        code_assistant_core::tools::default_registry(),
    )));

    // Create the session command service. The GUI gets the handle; the
    // worker runs on the backend tokio runtime below.
    let ui: Arc<dyn UserInterface> = Arc::new(gui.clone());
    let (service, service_worker) = SessionService::new(
        multi_session_manager,
        Arc::new(AgentRuntimeOptions {
            record_path: config.record.clone(),
            playback_path: config.playback.clone(),
            fast_playback: config.fast_playback,
            command_executor_factory: super::session_command_executor_factory(),
        }),
        ui,
    );
    gui.set_session_service(service.clone());

    let gui_for_thread = gui.clone();
    let task = config.task.clone();

    // Start the backend thread: runs the service worker and the startup
    // session connection on its own tokio runtime.
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();

        runtime.block_on(async {
            let worker = tokio::spawn(service_worker);

            startup(&service, &gui_for_thread, task).await;

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

            // Keep the runtime alive for the lifetime of the app.
            let _ = worker.await;
        });
    });

    // Run the GUI in the main thread
    gui.run_app();

    Ok(())
}

/// Connect the initial session: either create one for a provided task and
/// start the agent, or connect to the latest existing session.
async fn startup(service: &SessionService, gui: &ui_gpui::Gpui, task: Option<String>) {
    let session_id = if let Some(initial_task) = task {
        // Task provided - create a new session and start the agent for it
        debug!("Creating initial session with task");
        let session_id = match service.create_session(None, None).await {
            Ok(id) => id,
            Err(e) => {
                error!("Failed to create initial session: {e:#}");
                return;
            }
        };
        if let Err(e) = service.load_session(session_id.clone(), None).await {
            error!("Failed to connect initial session: {e:#}");
            return;
        }
        if let Err(e) = service
            .send_user_message(session_id.clone(), initial_task, Vec::new(), None)
            .await
        {
            error!("Failed to start agent with initial task: {e:#}");
        }
        Some(session_id)
    } else {
        // No task - connect to the latest existing session, if any
        info!("No task provided, connecting to latest session");
        let latest = service
            .list_sessions()
            .await
            .ok()
            .and_then(|sessions| sessions.first().map(|s| s.id.clone()));

        match latest {
            Some(session_id) => {
                debug!("Connecting to existing session: {}", session_id);
                // If the session's draft is in edit mode, connect with the
                // transcript already truncated to the branch parent so the
                // edit view is restored directly on startup.
                let edit_until_node_id = gui
                    .load_draft_for_session(&session_id)
                    .and_then(|(_, _, anchor)| anchor);
                if let Err(e) = service
                    .load_session(session_id.clone(), edit_until_node_id)
                    .await
                {
                    error!("Failed to connect to session {session_id}: {e:#}");
                    return;
                }
                Some(session_id)
            }
            None => {
                info!("No existing sessions found - showing empty state (no session view)");
                // In GPUI mode, don't auto-create a session. The user can
                // create one from the sidebar. The MessagesView will render
                // the "no session" hint since no session is connected.
                None
            }
        }
    };

    // Populate the skill catalog for the `/skill` input-area popup.
    if let Some(session_id) = session_id {
        match service.list_skills(session_id).await {
            Ok(skills) => gui.set_skills(skills),
            Err(e) => debug!("Failed to list skills at startup: {e:#}"),
        }
    }
}
