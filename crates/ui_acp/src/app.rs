use code_assistant_core::config::AgentRunConfig;

use crate::agent::AgentState;
use crate::ui::SessionUpdateMessage;
use crate::ACPUserUI;
use agent_client_protocol::{self as acp, Agent, Stdio};
use anyhow::Result;
use code_assistant_core::persistence::FileSessionPersistence;
use code_assistant_core::session::watcher::SessionWatcher;
use code_assistant_core::session::{SessionConfig, SessionManager};
use code_assistant_core::ui::ui_events::UiEvent;
use code_assistant_core::ui::UserInterface;

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

/// Run the ACP agent against stdio.
///
/// The SDK connection is `Send`, so the notification forwarder, the
/// filesystem-watcher handler, and the per-prompt agent tasks all run as
/// ordinary `tokio` tasks (no `LocalSet`/`spawn_local`).
pub async fn run(verbose: bool, config: AgentRunConfig) -> Result<()> {
    // Setup logging to file since stdout is used for ACP protocol
    use tracing_subscriber::prelude::*;

    let log_path = if cfg!(unix) {
        "/tmp/code-assistant-acp.log"
    } else {
        "code-assistant-acp.log"
    };

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .unwrap_or_else(|_| panic!("Failed to open log file at {log_path}"));

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(Arc::new(log_file))
                .with_ansi(false),
        )
        .with(tracing_subscriber::EnvFilter::new(if verbose {
            "debug"
        } else {
            "info"
        }))
        .init();

    info!(
        "Starting ACP agent mode (SDK 0.14), logging to {}",
        log_path
    );

    let session_config_template = SessionConfig {
        init_path: Some(config.path.canonicalize()?),
        initial_project: String::new(),
        tool_syntax: config.tool_syntax,
        use_diff_blocks: config.use_diff_format,
        sandbox_policy: config.sandbox_policy.clone(),
        ..SessionConfig::default()
    };

    // Model name has already been validated during CLI parsing
    let model_name = config.model.clone();

    // Create session manager
    let persistence = FileSessionPersistence::new();
    let persistence_for_watcher = FileSessionPersistence::new();
    let tool_registry = code_assistant_core::tools::default_registry();
    let events = code_assistant_core::session::event_stream::EventStream::new();
    let session_manager = Arc::new(Mutex::new(SessionManager::new(
        persistence,
        session_config_template.clone(),
        model_name.clone(),
        tool_registry.clone(),
        events.clone(),
    )));

    // Channel for session notifications: `ACPUserUI` instances push into the
    // sender; the forwarding task (below) drains and sends to the client.
    let (session_update_tx, session_update_rx) = mpsc::unbounded_channel::<SessionUpdateMessage>();

    // Connected session ID for the filesystem watcher.
    let connected_session_id: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));

    let state = Arc::new(AgentState::new(
        session_manager.clone(),
        session_config_template,
        model_name.clone(),
        tool_registry.clone(),
        config.playback.clone(),
        config.fast_playback,
        session_update_tx.clone(),
        connected_session_id.clone(),
    ));

    // Route the core→UI broadcast stream to the per-session ACP UIs: each
    // running prompt registers its ACPUserUI in `active_uis`; events for
    // sessions without an active prompt are dropped (ACP has no view to
    // update outside a prompt turn).
    {
        let active_uis = state.active_uis();
        let mut subscription = events.subscribe();
        tokio::spawn(async move {
            use code_assistant_core::session::{EventPayload, StreamError};
            loop {
                match subscription.recv().await {
                    Ok(event) => {
                        let Some(session_id) = &event.session_id else {
                            continue;
                        };
                        let ui = active_uis.lock().await.get(session_id).cloned();
                        let Some(ui) = ui else {
                            continue;
                        };
                        match event.payload {
                            EventPayload::Fragment(fragment) => {
                                let _ = ui.display_fragment(&fragment);
                            }
                            EventPayload::Ui(ui_event) => {
                                let _ = ui.send_event(ui_event).await;
                            }
                        }
                    }
                    Err(StreamError::Lagged { missed }) => {
                        warn!("ACP event stream lagged ({missed} events missed)");
                    }
                    Err(StreamError::Closed) => break,
                }
            }
        });
    }

    // Start the filesystem watcher for cross-instance awareness.
    let (watcher_event_tx, watcher_event_rx) = async_channel::bounded::<UiEvent>(64);
    let _session_watcher = match SessionWatcher::start(
        &persistence_for_watcher,
        watcher_event_tx,
        connected_session_id.clone(),
    ) {
        Ok(watcher) => {
            info!("Filesystem session watcher started (ACP mode)");
            Some(watcher)
        }
        Err(e) => {
            warn!("Failed to start filesystem session watcher: {e}");
            None
        }
    };

    // Forward cross-instance session changes to the ACP client.
    {
        let session_manager_for_watcher = session_manager.clone();
        let session_update_tx_for_watcher = session_update_tx.clone();
        let connected_session_id_for_watcher = connected_session_id.clone();
        let tool_registry_for_watcher = tool_registry.clone();
        let active_uis_for_watcher = state.active_uis();
        tokio::spawn(async move {
            handle_watcher_events(
                watcher_event_rx,
                session_manager_for_watcher,
                session_update_tx_for_watcher,
                connected_session_id_for_watcher,
                tool_registry_for_watcher,
                active_uis_for_watcher,
            )
            .await;
        });
    }

    // Build the agent connection with one handler per request type.
    let builder = Agent
        .builder()
        .name("code-assistant")
        .on_receive_request(
            {
                let state = state.clone();
                async move |req: acp::schema::InitializeRequest, responder, _cx| {
                    responder.respond_with_result(state.handle_initialize(req).await)
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let state = state.clone();
                async move |req: acp::schema::AuthenticateRequest, responder, _cx| {
                    responder.respond_with_result(state.handle_authenticate(req).await)
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let state = state.clone();
                async move |req: acp::schema::NewSessionRequest, responder, _cx| {
                    responder.respond_with_result(state.handle_new_session(req).await)
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let state = state.clone();
                async move |req: acp::schema::LoadSessionRequest, responder, _cx| {
                    responder.respond_with_result(state.handle_load_session(req).await)
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let state = state.clone();
                async move |req: acp::schema::SetSessionConfigOptionRequest, responder, _cx| {
                    responder.respond_with_result(state.handle_set_config_option(req).await)
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let state = state.clone();
                async move |req: acp::schema::ListSessionsRequest, responder, _cx| {
                    responder.respond_with_result(state.handle_list_sessions(req).await)
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let state = state.clone();
                async move |req: acp::schema::PromptRequest, responder, cx| {
                    // Run the turn in a detached task so the dispatch loop stays
                    // free to process `session/cancel` while the agent runs.
                    let state = state.clone();
                    tokio::spawn(async move {
                        let result = state.run_prompt(cx, req).await;
                        let _ = responder.respond_with_result(result);
                    });
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            {
                let state = state.clone();
                async move |notif: acp::schema::CancelNotification, _cx| {
                    state.handle_cancel(notif).await
                }
            },
            agent_client_protocol::on_receive_notification!(),
        );

    // Run the connection. `connect_with` drives the dispatch loop and the
    // `main_fn`; we use `main_fn` to forward queued notifications to the client
    // for the lifetime of the connection.
    let mut session_update_rx = session_update_rx;
    builder
        .connect_with(Stdio::new(), async move |conn| {
            while let Some((notification, ack)) = session_update_rx.recv().await {
                if let Err(e) = conn.send_notification(notification) {
                    tracing::error!("Failed to send session notification: {e}");
                    break;
                }
                let _ = ack.send(());
            }
            Ok::<(), acp::Error>(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("ACP connection error: {e}"))
}

/// Background task that processes filesystem watcher events.
async fn handle_watcher_events(
    event_rx: async_channel::Receiver<UiEvent>,
    session_manager: Arc<Mutex<SessionManager>>,
    session_update_tx: mpsc::UnboundedSender<SessionUpdateMessage>,
    connected_session_id: Arc<StdMutex<Option<String>>>,
    tool_registry: std::sync::Arc<tools_core::ToolRegistry>,
    active_uis: Arc<Mutex<HashMap<String, Arc<ACPUserUI>>>>,
) {
    while let Ok(event) = event_rx.recv().await {
        match event {
            UiEvent::RefreshCurrentSession { session_id } => {
                debug!("ACP watcher: RefreshCurrentSession for {session_id}");

                // Make sure this is still the connected session
                let current = connected_session_id.lock().unwrap().clone();
                if current.as_deref() != Some(&session_id) {
                    debug!(
                        "ACP watcher: ignoring refresh for {session_id} \
                         (connected session is {:?})",
                        current
                    );
                    continue;
                }

                // If we are actively streaming this session ourselves, the live
                // path already delivers all content. Replaying the watcher's
                // incremental diff here would duplicate the just-streamed
                // assistant message (the multi-instance awareness firing on our
                // own writes). Cross-instance updates still flow once the local
                // prompt finishes and the session leaves `active_uis`.
                if active_uis.lock().await.contains_key(&session_id) {
                    debug!(
                        "ACP watcher: ignoring refresh for {session_id} \
                         (locally streaming prompt in progress)"
                    );
                    continue;
                }

                let ui_events = {
                    let mut manager = session_manager.lock().await;
                    match manager.refresh_session_incremental(&session_id) {
                        Ok(events) => events,
                        Err(e) => {
                            warn!("ACP watcher: failed to refresh session {session_id}: {e}");
                            continue;
                        }
                    }
                };

                if ui_events.is_empty() {
                    continue;
                }

                let base_path = {
                    let manager = session_manager.lock().await;
                    manager
                        .get_session(&session_id)
                        .and_then(|s| s.session.config.init_path.clone())
                };

                let replay_ui = ACPUserUI::new(
                    acp::schema::SessionId::new(session_id.clone()),
                    session_update_tx.clone(),
                    base_path,
                    tool_registry.clone(),
                    None,
                );

                for ui_event in ui_events {
                    if let Err(e) = replay_ui.send_event(ui_event).await {
                        warn!("ACP watcher: failed to send event for {session_id}: {e}");
                    }
                }
            }

            UiEvent::UpdateSessionActivityState {
                session_id,
                activity_state,
            } => {
                debug!(
                    "ACP watcher: UpdateSessionActivityState for {session_id}: {activity_state:?}"
                );

                let mut manager = session_manager.lock().await;
                if let Some(instance) = manager.get_session_mut(&session_id) {
                    instance.set_activity_state(activity_state);
                }
            }

            UiEvent::RefreshChatList => {
                debug!("ACP watcher: RefreshChatList (ignored, client uses list_sessions)");
            }

            _ => {
                debug!("ACP watcher: unexpected event: {:?}", event);
            }
        }
    }
}
