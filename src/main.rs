mod agent;
mod explorer;
mod llm;
mod types;
mod ui;
mod utils;

use crate::agent::Agent;
use crate::explorer::Explorer;
use crate::llm::{AnthropicClient, LLMProvider, OllamaClient, OpenAIClient};
use crate::ui::terminal::TerminalUI;
use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use std::path::PathBuf;
use tracing::Level;

#[derive(ValueEnum, Debug, Clone)]
enum LLMProviderType {
    Anthropic,
    OpenAI,
    Ollama,
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to the code directory to analyze
    #[arg(long, default_value = ".")]
    path: PathBuf,

    /// Task to perform on the codebase
    #[arg(short, long)]
    task: String,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// LLM provider to use
    #[arg(short = 'p', long, default_value = "anthropic")]
    provider: LLMProviderType,

    /// Model name to use (provider-specific)
    #[arg(short = 'm', long)]
    model: Option<String>,

    /// Context window size (in tokens, only relevant for Ollama)
    #[arg(long, default_value = "8192")]
    num_ctx: usize,
}

fn create_llm_client(args: &Args) -> Result<Box<dyn LLMProvider>> {
    match args.provider {
        LLMProviderType::Anthropic => {
            let api_key = std::env::var("ANTHROPIC_API_KEY")
                .context("ANTHROPIC_API_KEY environment variable not set")?;

            Ok(Box::new(AnthropicClient::new(
                api_key,
                args.model
                    .clone()
                    .unwrap_or_else(|| "claude-3-5-sonnet-20241022".to_string()),
            )))
        }

        LLMProviderType::OpenAI => {
            let api_key = std::env::var("OPENAI_API_KEY")
                .context("OPENAI_API_KEY environment variable not set")?;

            Ok(Box::new(OpenAIClient::new(
                api_key,
                args.model.clone().unwrap_or_else(|| "gpt-4o".to_string()),
            )))
        }

        LLMProviderType::Ollama => Ok(Box::new(OllamaClient::new(
            args.model
                .clone()
                .context("Model name is required for Ollama provider")?,
            args.num_ctx,
        ))),
    }
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

    // Setup LLM client with the specified provider
    let llm_client = create_llm_client(&args).context("Failed to initialize LLM client")?;

    // Setup CodeExplorer
    let root_path = args.path.canonicalize()?;
    let explorer = Box::new(Explorer::new(root_path));

    // Initialize terminal UI
    let terminal_ui = Box::new(TerminalUI::new());

    // Initialize agent
    let mut agent = Agent::new(llm_client, explorer, terminal_ui);

    // Start agent with the specified task
    agent.start(args.task).await?;

    Ok(())
}
