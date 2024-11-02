mod agent;
mod api_client;
mod explorer;
mod llm;
mod types;

use crate::agent::Agent;
use crate::llm::AnthropicClient;
use anyhow::Result;
use std::path::PathBuf;
use tracing::Level;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize detailed logging
    tracing_subscriber::fmt()
        .with_max_level(Level::DEBUG)
        .with_target(false) // Removes module path from output
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true)
        .with_level(true)
        .pretty()
        .init();

    // Setup LLM client
    let llm_client = AnthropicClient::new(
        std::env::var("ANTHROPIC_API_KEY")?,
        "claude-3-5-sonnet-20241022".to_string(),
    );

    // Initialize agent
    let root_dir = PathBuf::from("./");
    let mut agent = Agent::new(Box::new(llm_client), root_dir);

    // Start agent with a task
    agent
        .start("Analyze this codebase and tell me what the main components are".to_string())
        .await?;

    Ok(())
}
