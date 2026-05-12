use super::AgentRunConfig;
use crate::acp::{
    register_fs_worker, register_terminal_worker, set_acp_client_connection, ACPAgentImpl,
    ACPUserUI,
};
use crate::persistence::FileSessionPersistence;
use crate::session::watcher::SessionWatcher;
use crate::session::{SessionConfig, SessionManager};
use crate::ui::ui_events::UiEvent;
use crate::ui::UserInterface;
use agent_client_protocol as acp;
use agent_client_protocol::Client;
use anyhow::Result;

use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing::{debug, info, warn};

pub async fn run(verbose: bool, config: AgentRunConfig) -> Result<()> {
    // Setup logging to file since stdout is used for ACP protocol
    use tracing_subscriber::prelude::*;

    // Use /tmp on Unix-like systems
    let log_path = if cfg!(unix) {
        "/tmp/code-assistant-acp.log"
    } else {
        // Windows fallback
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

    info!("Starting ACP agent mode, logging to {}", log_path);

    // Prepare configuration

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
    let session_manager = Arc::new(Mutex::new(SessionManager::new(
        persistence,
        session_config_template.clone(),
        model_name.clone(),
    )));

    // Setup stdio transport
    let outgoing = tokio::io::stdout().compat_write();
    let incoming = tokio::io::stdin().compat();

    // Create channel for session notifications
    let (session_update_tx, mut session_update_rx) = mpsc::unbounded_channel();

    // Connected session ID for the filesystem watcher
    let connected_session_id: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));

    // Create the agent
    let agent = ACPAgentImpl::new(
        session_manager.clone(),
        session_config_template,
        model_name.clone(),
        config.playback.clone(),
        config.fast_playback,
        session_update_tx.clone(),
        connected_session_id.clone(),
    );

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

    // Use LocalSet for non-Send futures from agent-client-protocol,
    // but the spawned futures will themselves spawn agent tasks on the multi-threaded runtime
    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async move {
            // Create the ACP connection
            let (conn, handle_io) =
                agent_client_protocol::AgentSideConnection::new(agent, outgoing, incoming, |fut| {
                    // Spawn on LocalSet for agent-client-protocol futures
                    tokio::task::spawn_local(fut);
                });

            // Set the global connection for use by ACP components
            let conn_arc = Arc::new(conn);
            set_acp_client_connection(conn_arc.clone());
            register_terminal_worker(conn_arc.clone());
            register_fs_worker(conn_arc.clone());

            // Kick off a background task to send session notifications to the client
            let conn_for_notifications = conn_arc.clone();
            tokio::task::spawn_local(async move {
                while let Some((session_notification, tx)) = session_update_rx.recv().await {
                    let result = conn_for_notifications
                        .session_notification(session_notification)
                        .await;
                    if let Err(e) = result {
                        tracing::error!("Failed to send session notification: {}", e);
                        break;
                    }
                    tx.send(()).ok();
                }
            });

            // Kick off a background task to handle filesystem watcher events
            // and forward cross-instance session changes to the ACP client
            let session_manager_for_watcher = session_manager.clone();
            let session_update_tx_for_watcher = session_update_tx.clone();
            let connected_session_id_for_watcher = connected_session_id.clone();
            tokio::task::spawn_local(async move {
                handle_watcher_events(
                    watcher_event_rx,
                    session_manager_for_watcher,
                    session_update_tx_for_watcher,
                    connected_session_id_for_watcher,
                )
                .await;
            });

            // Run the IO handler until stdin/stdout are closed
            handle_io.await
        })
        .await
        .map_err(anyhow::Error::new)
}

/// Background task that processes filesystem watcher events and replays
/// incremental session changes to the ACP client.
///
/// When another code-assistant instance modifies the currently connected
/// session's file on disk, this task:
/// 1. Calls `refresh_session_incremental` to compute the diff
/// 2. Converts new messages/fragments to ACP `SessionNotification`s
/// 3. Sends them through the existing notification channel
async fn handle_watcher_events(
    event_rx: async_channel::Receiver<UiEvent>,
    session_manager: Arc<Mutex<SessionManager>>,
    session_update_tx: mpsc::UnboundedSender<(acp::SessionNotification, oneshot::Sender<()>)>,
    connected_session_id: Arc<StdMutex<Option<String>>>,
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

                // Compute incremental diff
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

                // Replay the resulting UI events as ACP notifications
                for ui_event in ui_events {
                    replay_ui_event_to_acp(
                        &ui_event,
                        &session_id,
                        &session_manager,
                        &session_update_tx,
                    )
                    .await;
                }
            }

            UiEvent::UpdateSessionActivityState {
                session_id,
                activity_state,
            } => {
                debug!(
                    "ACP watcher: UpdateSessionActivityState for {session_id}: {activity_state:?}"
                );

                // Update the state in the session manager
                let mut manager = session_manager.lock().await;
                if let Some(instance) = manager.get_session_mut(&session_id) {
                    instance.set_activity_state(activity_state);
                }
                // Note: ACP protocol doesn't have a direct "activity state" notification,
                // but the client can observe that new content is streaming in.
            }

            UiEvent::RefreshChatList => {
                // In ACP mode the client manages its own session list via list_sessions().
                // We could potentially notify the client to re-fetch, but the protocol
                // doesn't currently have a mechanism for this.
                debug!("ACP watcher: RefreshChatList (ignored, client uses list_sessions)");
            }

            _ => {
                // Other events are not expected from the watcher
                debug!("ACP watcher: unexpected event: {:?}", event);
            }
        }
    }
}

/// Convert a UiEvent from `refresh_session_incremental` into ACP session
/// notifications and send them to the client.
async fn replay_ui_event_to_acp(
    event: &UiEvent,
    session_id: &str,
    session_manager: &Arc<Mutex<SessionManager>>,
    session_update_tx: &mpsc::UnboundedSender<(acp::SessionNotification, oneshot::Sender<()>)>,
) {
    match event {
        UiEvent::AppendMessages {
            messages,
            tool_results,
        } => {
            let acp_session_id = acp::SessionId::new(session_id);

            // Get base_path for the session
            let base_path = {
                let manager = session_manager.lock().await;
                manager
                    .get_session(session_id)
                    .and_then(|s| s.session.config.init_path.clone())
            };

            // Create a temporary ACPUserUI to replay fragments through.
            // This reuses the same fragment→ACP conversion logic used during
            // streaming and load_session replay.
            let replay_ui = Arc::new(ACPUserUI::new(
                acp_session_id.clone(),
                session_update_tx.clone(),
                base_path,
            ));

            // Replay message fragments
            for message_data in messages {
                use crate::ui::gpui::elements::MessageRole;

                match message_data.role {
                    MessageRole::User => {
                        // Emit user message fragments as UserMessageChunk
                        for fragment in &message_data.fragments {
                            match fragment {
                                crate::ui::DisplayFragment::PlainText(text) => {
                                    let content = acp::ContentBlock::Text(acp::TextContent::new(
                                        text.clone(),
                                    ));
                                    let chunk = ACPUserUI::content_chunk(content);
                                    replay_ui.queue_session_update(
                                        acp::SessionUpdate::UserMessageChunk(chunk),
                                    );
                                }
                                crate::ui::DisplayFragment::CompactionDivider { summary } => {
                                    let content = acp::ContentBlock::Text(acp::TextContent::new(
                                        format!("[Context compacted: {}]", summary),
                                    ));
                                    let chunk = ACPUserUI::content_chunk(content);
                                    replay_ui.queue_session_update(
                                        acp::SessionUpdate::AgentMessageChunk(chunk),
                                    );
                                }
                                _ => {
                                    // Other fragment types in user messages are uncommon
                                }
                            }
                        }
                    }
                    MessageRole::Assistant => {
                        // Emit assistant message fragments through display_fragment
                        // which handles all the ACP conversion logic
                        for fragment in &message_data.fragments {
                            if let Err(e) = replay_ui.display_fragment(fragment) {
                                warn!(
                                    "ACP watcher: failed to replay fragment for {session_id}: {e}"
                                );
                            }
                        }
                    }
                }
            }

            // Replay tool results as ToolCallUpdate with final status
            for tool_result in tool_results {
                let status = match tool_result.status {
                    crate::ui::ToolStatus::Success => acp::ToolCallStatus::Completed,
                    crate::ui::ToolStatus::Error => acp::ToolCallStatus::Failed,
                    _ => acp::ToolCallStatus::InProgress,
                };

                let output_content: Vec<acp::ToolCallContent> = tool_result
                    .output
                    .as_ref()
                    .map(|o| {
                        vec![acp::ToolCallContent::Content(acp::Content::new(
                            acp::ContentBlock::Text(acp::TextContent::new(o.clone())),
                        ))]
                    })
                    .unwrap_or_default();

                let mut update_fields = acp::ToolCallUpdateFields::new().status(status);
                if !output_content.is_empty() {
                    update_fields = update_fields.content(output_content);
                }

                let tool_call_update = acp::ToolCallUpdate::new(
                    acp::ToolCallId::new(tool_result.tool_id.clone()),
                    update_fields,
                );

                replay_ui
                    .queue_session_update(acp::SessionUpdate::ToolCallUpdate(tool_call_update));
            }
        }

        UiEvent::SetMessages { .. } => {
            // A full reload is needed (paths diverged). In ACP mode we can't easily
            // "clear and reload" the session from the agent side without a protocol-level
            // mechanism. For now, log a warning. The client would need to call
            // load_session again to get the full state.
            warn!(
                "ACP watcher: session {session_id} paths diverged — \
                 full reload required but not yet supported in ACP mode"
            );
        }

        UiEvent::UpdatePlan { plan } => {
            use crate::types::{PlanItemPriority, PlanItemStatus};

            // Forward plan updates to the ACP client
            let acp_session_id = acp::SessionId::new(session_id);
            if !plan.entries.is_empty() {
                let acp_entries: Vec<acp::PlanEntry> = plan
                    .entries
                    .iter()
                    .map(|item| {
                        let status = match item.status {
                            PlanItemStatus::Completed => acp::PlanEntryStatus::Completed,
                            PlanItemStatus::InProgress => acp::PlanEntryStatus::InProgress,
                            PlanItemStatus::Pending => acp::PlanEntryStatus::Pending,
                        };
                        let priority = match item.priority {
                            PlanItemPriority::High => acp::PlanEntryPriority::High,
                            PlanItemPriority::Low => acp::PlanEntryPriority::Low,
                            PlanItemPriority::Medium => acp::PlanEntryPriority::Medium,
                        };
                        acp::PlanEntry::new(item.content.clone(), priority, status)
                    })
                    .collect();

                let plan_update = acp::Plan::new(acp_entries);
                let notification = acp::SessionNotification::new(
                    acp_session_id,
                    acp::SessionUpdate::Plan(plan_update),
                );
                let (ack_tx, _) = oneshot::channel();
                if let Err(e) = session_update_tx.send((notification, ack_tx)) {
                    warn!("ACP watcher: failed to send plan update: {e}");
                }
            }
        }

        _ => {
            debug!(
                "ACP watcher: unhandled UI event during replay: {:?}",
                std::mem::discriminant(event)
            );
        }
    }
}
