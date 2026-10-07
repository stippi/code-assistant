#[cfg(feature = "acp-frontend")]
pub mod acp;
#[cfg(feature = "gpui-frontend")]
pub mod gpui;
#[cfg(feature = "mcp-server")]
pub mod server;
#[cfg(feature = "terminal-frontend")]
pub mod terminal;

pub use code_assistant_core::config::AgentRunConfig;

/// Move sessions stored in the old flat layout into session folders. Runs
/// before any frontend touches the session store; a failure is logged and
/// leaves the remaining old sessions for the next start.
pub fn migrate_session_store() {
    // Where `FileDraftStore` kept drafts before they moved into the session
    // folders.
    let legacy_drafts_dir = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("code-assistant")
        .join("drafts");
    let mut persistence = code_assistant_core::persistence::FileSessionPersistence::new();
    match persistence.migrate_legacy_sessions(&legacy_drafts_dir) {
        Ok(report) => {
            if !report.migrated.is_empty() {
                tracing::info!("Migrated {} sessions", report.migrated.len());
            }
            for (session_id, reason) in &report.skipped {
                tracing::warn!("Session {session_id} not migrated: {reason}");
            }
        }
        Err(e) => tracing::warn!("Session migration failed: {e:#}"),
    }
}

#[cfg(any(feature = "gpui-frontend", feature = "terminal-frontend"))]
use code_assistant_core::session::service::CommandExecutorFactory;

/// The command executor the GPUI frontend uses for agent sessions:
/// commands run on a backend PTY whose raw (colored) output streams to the
/// terminal cards as display fragments — the UI never sits between the
/// agent loop and the process.
#[cfg(feature = "gpui-frontend")]
pub fn session_command_executor_factory() -> CommandExecutorFactory {
    std::sync::Arc::new(|_session_id: &str| Box::new(command_executor::PtyCommandExecutor))
}

/// Without the GPUI frontend there are no terminal cards; commands run
/// through the plain executor.
#[cfg(all(feature = "terminal-frontend", not(feature = "gpui-frontend")))]
pub fn session_command_executor_factory() -> CommandExecutorFactory {
    std::sync::Arc::new(|_session_id: &str| Box::new(command_executor::DefaultCommandExecutor))
}
