pub mod acp;
#[cfg(feature = "gpui-frontend")]
pub mod gpui;
pub mod server;
#[cfg(feature = "terminal-frontend")]
pub mod terminal;

pub use code_assistant_core::config::AgentRunConfig;

#[cfg(any(feature = "gpui-frontend", feature = "terminal-frontend"))]
use code_assistant_core::backend::CommandExecutorFactory;

/// The command executor the interactive frontends use for agent sessions:
/// commands run attached to live terminal views when the GPUI terminal pool
/// is available and fall back to plain execution otherwise.
#[cfg(feature = "gpui-frontend")]
pub fn session_command_executor_factory() -> CommandExecutorFactory {
    std::sync::Arc::new(|session_id: &str| {
        Box::new(ui_gpui::terminal::executor::GpuiTerminalCommandExecutor::new(
            session_id.to_string(),
        ))
    })
}

/// Without the GPUI frontend there are no terminal views to attach to;
/// commands always run through the plain executor (the same path the GPUI
/// executor falls back to when no terminal worker is available).
#[cfg(all(feature = "terminal-frontend", not(feature = "gpui-frontend")))]
pub fn session_command_executor_factory() -> CommandExecutorFactory {
    std::sync::Arc::new(|_session_id: &str| Box::new(command_executor::DefaultCommandExecutor))
}
