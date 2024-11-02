mod agent;
mod api_client;
mod explorer;
mod llm;
mod types;

use crate::agent::Agent;
use crate::llm::AnthropicClient;
use anyhow::Result;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Setup LLM client
    let llm_client = AnthropicClient::new(
        std::env::var("ANTHROPIC_API_KEY")?,
        "claude-3-5-sonnet-20241022".to_string(),
    );

    // Initialize agent
    let root_dir = PathBuf::from("./test-repo");
    let mut agent = Agent::new(Box::new(llm_client), root_dir);

    // Start agent with a task
    agent
        .start("Analyze this codebase and tell me what the main components are".to_string())
        .await?;

    Ok(())
}
