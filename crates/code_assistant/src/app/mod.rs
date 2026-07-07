#[cfg(feature = "acp-frontend")]
pub mod acp;
#[cfg(feature = "gpui-frontend")]
pub mod gpui;
#[cfg(feature = "mcp-server")]
pub mod server;
#[cfg(feature = "terminal-frontend")]
pub mod terminal;

pub use code_assistant_core::config::AgentRunConfig;

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
