use super::AgentRunConfig;
use anyhow::Result;

pub async fn run(verbose: bool, config: AgentRunConfig) -> Result<()> {
    ui_acp::run(verbose, config).await
}
