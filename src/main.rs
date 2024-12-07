mod agent;
mod explorer;
mod llm;
mod mcp;
mod types;
mod ui;
mod utils;

use crate::agent::Agent;
use crate::explorer::Explorer;
use crate::llm::{AnthropicClient, LLMProvider, OllamaClient, OpenAIClient};
use crate::mcp::MCPServer;
use crate::ui::terminal::TerminalUI;
use crate::utils::DefaultCommandExecutor;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::io;
use std::path::PathBuf;
use tracing::Level;
use tracing_subscriber::fmt::SubscriberBuilder;

#[derive(ValueEnum, Debug, Clone)]
enum LLMProviderType {
    Anthropic,
    OpenAI,
    Ollama,
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    mode: Mode,
}

#[derive(Subcommand, Debug)]
enum Mode {
    /// Run as autonomous agent with LLM support
    Agent {
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
    },
    /// Run as MCP server
    Server {
        /// Path to the code directory to serve
        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Enable verbose logging
        #[arg(short, long)]
        verbose: bool,
    },
}

fn create_llm_client(
    provider: LLMProviderType,
    model: Option<String>,
    num_ctx: usize,
) -> Result<Box<dyn LLMProvider>> {
    match provider {
        LLMProviderType::Anthropic => {
            let api_key = std::env::var("ANTHROPIC_API_KEY")
                .context("ANTHROPIC_API_KEY environment variable not set")?;

            Ok(Box::new(AnthropicClient::new(
                api_key,
                model
                    .clone()
                    .unwrap_or_else(|| "claude-3-5-sonnet-20241022".to_string()),
            )))
        }

        LLMProviderType::OpenAI => {
            let api_key = std::env::var("OPENAI_API_KEY")
                .context("OPENAI_API_KEY environment variable not set")?;

            Ok(Box::new(OpenAIClient::new(
                api_key,
                model.clone().unwrap_or_else(|| "gpt-4o".to_string()),
            )))
        }

        LLMProviderType::Ollama => Ok(Box::new(OllamaClient::new(
            model
                .clone()
                .context("Model name is required for Ollama provider")?,
            num_ctx,
        ))),
    }
}

fn setup_logging(verbose: bool, use_stdout: bool) {
    let log_level = if verbose { Level::DEBUG } else { Level::INFO };

    let subscriber = tracing_subscriber::fmt()
        .with_max_level(log_level)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true)
        .with_level(true);

    // For server mode, write only to stderr to keep stdout clean for JSON-RPC
    let subscriber: SubscriberBuilder<_, _, _, fn() -> Box<dyn io::Write + Send>> = if use_stdout {
        subscriber.with_writer(|| Box::new(std::io::stdout()) as Box<dyn io::Write + Send>)
    } else {
        subscriber.with_writer(|| Box::new(std::io::stderr()) as Box<dyn io::Write + Send>)
    };

    subscriber.init();
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse command line arguments
    let args = Args::parse();

    match args.mode {
        Mode::Agent {
            path,
            task,
            verbose,
            provider,
            model,
            num_ctx,
        } => {
            // Setup logging based on verbose flag
            setup_logging(verbose, true);

            // Ensure the path exists and is a directory
            if !path.is_dir() {
                anyhow::bail!("Path '{}' is not a directory", path.display());
            }

            // Setup LLM client with the specified provider
            let llm_client = create_llm_client(provider, model, num_ctx)
                .context("Failed to initialize LLM client")?;

            // Setup CodeExplorer
            let root_path = path.canonicalize()?;
            let explorer = Box::new(Explorer::new(root_path));

            // Initialize terminal UI
            let terminal_ui = Box::new(TerminalUI::new());
            let command_executor = Box::new(DefaultCommandExecutor);

            // Initialize agent
            let mut agent = Agent::new(llm_client, explorer, command_executor, terminal_ui);

            // Start agent with the specified task
            agent.start(task).await?;
        }

        Mode::Server { path, verbose } => {
            // Setup logging based on verbose flag
            setup_logging(verbose, false);

            // Canonicalize the path to get absolute path
            let root_path = path
                .canonicalize()
                .context("Failed to resolve project path")?;

            // Ensure the path exists and is a directory
            if !root_path.is_dir() {
                anyhow::bail!("Path '{}' is not a directory", root_path.display());
            }

            // Initialize server
            let mut server = MCPServer::new(root_path)?;
            server.run().await?;
        }
    }

    Ok(())
}
