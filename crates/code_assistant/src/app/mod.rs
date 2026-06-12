pub mod acp;
pub mod gpui;
pub mod server;
pub mod terminal;

pub use code_assistant_core::config::AgentRunConfig;

/// The command executor both interactive frontends use for agent sessions:
/// commands run attached to live terminal views when the GPUI terminal pool
/// is available and fall back to plain execution otherwise.
pub fn gpui_terminal_executor_factory() -> code_assistant_core::backend::CommandExecutorFactory {
    std::sync::Arc::new(|session_id: &str| {
        Box::new(ui_gpui::terminal::executor::GpuiTerminalCommandExecutor::new(
            session_id.to_string(),
        ))
    })
}
