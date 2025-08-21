use super::AgentRunConfig;
use crate::ui::terminal::TerminalApp;
use anyhow::Result;

pub async fn run(config: AgentRunConfig) -> Result<()> {
    // Use the new terminal UI implementation
    let terminal_app = TerminalApp::new();
    terminal_app.run(&config).await
}
