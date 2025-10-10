use super::AgentRunConfig;
use crate::acp::{register_terminal_worker, set_acp_client_connection, ACPAgentImpl};
use crate::persistence::FileSessionPersistence;
use crate::session::{SessionConfig, SessionManager};
use agent_client_protocol::Client;
use anyhow::Result;

use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing::info;

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

    let log_file = std::fs::File::create(log_path)
        .unwrap_or_else(|_| panic!("Failed to create log file at {log_path}"));

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
    };

    // Get model name or use default
    let model_name = config
        .model
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Model name is required for ACP mode"))?;

    // Create session manager
    let persistence = FileSessionPersistence::new();
    let session_manager = Arc::new(Mutex::new(SessionManager::new(
        persistence,
        session_config_template.clone(),
    )));

    // Setup stdio transport
    let outgoing = tokio::io::stdout().compat_write();
    let incoming = tokio::io::stdin().compat();

    // Create channel for session notifications
    let (session_update_tx, mut session_update_rx) = mpsc::unbounded_channel();

    // Create the agent
    let agent = ACPAgentImpl::new(
        session_manager,
        session_config_template,
        model_name.clone(),
        config.playback.clone(),
        config.fast_playback,
        session_update_tx,
    );

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

            // Run the IO handler until stdin/stdout are closed
            handle_io.await
        })
        .await
}
