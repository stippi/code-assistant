use super::AgentRunConfig;
use anyhow::Result;
use ui_terminal::TerminalApp;

pub async fn run(config: AgentRunConfig) -> Result<()> {
    // Use the new terminal UI implementation
    let terminal_app = TerminalApp::new();
    terminal_app
        .run(&config, super::session_command_executor_factory())
        .await
}
