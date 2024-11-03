mod agent;
mod explorer;
mod llm;
mod types;
mod ui;

use crate::agent::Agent;
use crate::llm::AnthropicClient;
use crate::ui::terminal::TerminalUI;
use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use tracing::Level;

/// AI-powered code analysis assistant
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to the code directory to analyze
    #[arg(short, long, default_value = ".")]
    path: PathBuf,

    /// Task to perform on the codebase
    #[arg(short, long)]
    task: String,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse command line arguments
    let args = Args::parse();

    // Setup logging based on verbose flag
    let log_level = if args.verbose {
        Level::DEBUG
    } else {
        Level::INFO
    };

    tracing_subscriber::fmt()
        .with_max_level(log_level)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true)
        .with_level(true)
        .pretty()
        .init();

    // Ensure the path exists and is a directory
    if !args.path.is_dir() {
        anyhow::bail!("Path '{}' is not a directory", args.path.display());
    }

    // Setup LLM client
    let llm_client = AnthropicClient::new(
        std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY environment variable not set"))?,
        "claude-3-5-sonnet-20241022".to_string(),
    );

    // Initialize terminal UI
    let terminal_ui = Box::new(TerminalUI::new());

    // Initialize agent
    let mut agent = Agent::new(
        Box::new(llm_client),
        args.path.canonicalize()?, // Convert to absolute path
        terminal_ui,
    );

    // Start agent with the specified task
    agent.start(args.task).await?;

    Ok(())
}
