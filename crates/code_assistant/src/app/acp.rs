use super::AgentRunConfig;
use crate::acp::ACPAgentImpl;
use crate::persistence::FileSessionPersistence;
use crate::session::{AgentConfig, SessionManager};
use anyhow::Result;
use llm::factory::LLMClientConfig;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing::info;

pub async fn run(verbose: bool, config: AgentRunConfig) -> Result<()> {
    // Setup logging
    crate::logging::setup_logging(if verbose { 1 } else { 0 }, false);

    info!("Starting ACP agent mode");

    // Prepare configuration
    let agent_config = AgentConfig {
        tool_syntax: config.tool_syntax,
        init_path: Some(config.path.canonicalize()?),
        initial_project: String::new(),
        use_diff_blocks: config.use_diff_format,
    };

    let llm_config = LLMClientConfig {
        provider: config.provider,
        model: config.model,
        base_url: config.base_url,
        aicore_config: config.aicore_config,
        num_ctx: config.num_ctx,
        record_path: config.record,
        playback_path: config.playback,
        fast_playback: config.fast_playback,
    };

    // Create session manager
    let persistence = FileSessionPersistence::new();
    let session_manager = Arc::new(Mutex::new(SessionManager::new(
        persistence,
        agent_config.clone(),
    )));

    // Setup stdio transport
    let outgoing = tokio::io::stdout().compat_write();
    let incoming = tokio::io::stdin().compat();

    // Create channel for session notifications
    let (session_update_tx, mut session_update_rx) = mpsc::unbounded_channel();

    // Create the agent
    let agent = ACPAgentImpl::new(session_manager, agent_config, llm_config, session_update_tx);

    // Use LocalSet for non-Send futures from agent-client-protocol
    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async move {
            // Create the ACP connection
            let (conn, handle_io) =
                agent_client_protocol::AgentSideConnection::new(agent, outgoing, incoming, |fut| {
                    tokio::task::spawn_local(fut);
                });

            // Kick off a background task to send session notifications to the client
            tokio::task::spawn_local(async move {
                while let Some((session_notification, tx)) = session_update_rx.recv().await {
                    let result = conn.session_notification(session_notification).await;
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
