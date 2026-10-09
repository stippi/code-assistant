#[cfg(feature = "acp-frontend")]
pub mod acp;
#[cfg(feature = "gpui-frontend")]
pub mod gpui;
#[cfg(feature = "mcp-server")]
pub mod server;
#[cfg(feature = "terminal-frontend")]
pub mod terminal;
#[cfg(feature = "voice")]
pub mod voice;

pub use code_assistant_core::config::AgentRunConfig;
use code_assistant_core::persistence::{FileSessionPersistence, MigrationProgress};

/// Move sessions stored in the old flat layout into session folders,
/// reporting `progress`. Runs before any frontend touches the session
/// store; a failure is logged and leaves the remaining old sessions for the
/// next start.
pub fn migrate_session_store(progress: &(dyn Fn(MigrationProgress) + Sync)) {
    // Where `FileDraftStore` kept drafts before they moved into the session
    // folders.
    let legacy_drafts_dir = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("code-assistant")
        .join("drafts");
    let mut persistence = FileSessionPersistence::new();
    match persistence.migrate_legacy_sessions(&legacy_drafts_dir, progress) {
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

/// [`migrate_session_store`] for frontends without a window of their own:
/// progress goes to stderr, on one line that is updated in place.
pub fn migrate_session_store_on_stderr() {
    use std::io::Write;
    if !FileSessionPersistence::new().has_legacy_sessions() {
        return;
    }
    let last_percent = std::sync::Mutex::new(None);
    migrate_session_store(&|progress| {
        let percent = (progress.fraction() * 100.0) as u32;
        let mut last = last_percent.lock().unwrap();
        if *last != Some(percent) {
            *last = Some(percent);
            eprint!(
                "\rMigrating sessions to the new storage format: {percent:3}% ({})",
                progress.phase.description()
            );
            let _ = std::io::stderr().flush();
        }
    });
    eprintln!();
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
