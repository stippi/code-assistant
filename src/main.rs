mod agent;
mod explorer;
mod llm;
mod mcp;
mod persistence;
mod tool_definitions;
mod tools;
mod types;
mod ui;
mod utils;

use crate::agent::{Agent, ToolMode};
use crate::explorer::Explorer;
use crate::llm::{AnthropicClient, LLMProvider, OllamaClient, OpenAIClient, VertexClient};
use crate::mcp::MCPServer;
use crate::ui::terminal::TerminalUI;
use crate::utils::DefaultCommandExecutor;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use persistence::FileStatePersistence;
use std::io;
use std::path::PathBuf;
use tracing_subscriber::fmt::SubscriberBuilder;

#[derive(ValueEnum, Debug, Clone)]
enum LLMProviderType {
    Anthropic,
    OpenAI,
    Ollama,
    Vertex,
}

#[derive(ValueEnum, Debug, Clone)]
enum ToolsType {
    Native,
    Xml,
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

        /// Task to perform on the codebase (required unless --continue is used)
        #[arg(short, long, required_unless_present = "continue_task")]
        task: Option<String>,

        /// Continue from previous state
        #[arg(long)]
        continue_task: bool,

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

        /// Type of tool declaration ('native' = tools via API, 'xml' = custom system message)
        #[arg(long)]
        tools_type: ToolsType,
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

        LLMProviderType::Vertex => {
            let api_key = std::env::var("GOOGLE_API_KEY")
                .context("GOOGLE_API_KEY environment variable not set")?;

            Ok(Box::new(VertexClient::new(
                api_key,
                model
                    .clone()
                    .unwrap_or_else(|| "gemini-1.5-pro-latest".to_string()),
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
    let filter = {
        if verbose {
            "code_assistant=debug,info".to_string()
        } else {
            "code_assistant=info,warn".to_string()
        }
    };

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
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
            continue_task,
            verbose,
            provider,
            model,
            num_ctx,
            tools_type,
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

            // Setup dynamic types
            let root_path = path.canonicalize()?;
            let explorer = Box::new(Explorer::new(root_path.clone()));
            let terminal_ui = Box::new(TerminalUI::new());
            let command_executor = Box::new(DefaultCommandExecutor);
            let state_persistence = Box::new(FileStatePersistence::new(root_path.clone()));

            // Validate parameters
            if continue_task && task.is_some() {
                anyhow::bail!(
                    "Cannot specify both --task and --continue. The task will be loaded from the saved state."
                );
            }

            if !continue_task && task.is_none() {
                anyhow::bail!("Either --task or --continue must be specified");
            }

            // Initialize agent
            let mut agent = Agent::new(
                llm_client,
                match &tools_type {
                    ToolsType::Native => ToolMode::Native,
                    ToolsType::Xml => ToolMode::Xml,
                },
                explorer,
                command_executor,
                terminal_ui,
                state_persistence,
            );

            // Get task either from state file or argument
            if continue_task {
                agent.start_from_state().await?;
            } else {
                agent.start_with_task(task.unwrap()).await?;
            }
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
