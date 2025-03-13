mod agent;
mod config;
mod explorer;
mod llm;
mod mcp;
mod persistence;
mod tests;
mod tools;
mod types;
mod ui;
mod utils;
mod web;

use crate::agent::Agent;
use crate::explorer::Explorer;
use crate::llm::{AnthropicClient, LLMProvider, OllamaClient, OpenAIClient, VertexClient};
use crate::mcp::MCPServer;
use crate::ui::terminal::TerminalUI;
use crate::ui::UserInterface;
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

// Define the application arguments
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    mode: Option<Mode>,

    /// Path to the code directory to analyze
    #[arg(long, default_value = ".")]
    path: Option<PathBuf>,

    /// Task to perform on the codebase (required unless --continue or --ui is used)
    #[arg(short, long, required_unless_present_any = ["continue_task", "ui"])]
    task: Option<String>,

    /// Start with GUI interface
    #[arg(long)]
    ui: bool,

    /// Continue from previous state
    #[arg(long)]
    continue_task: bool,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// LLM provider to use
    #[arg(short = 'p', long, default_value = "anthropic")]
    provider: Option<LLMProviderType>,

    /// Model name to use (provider-specific)
    #[arg(short = 'm', long)]
    model: Option<String>,

    /// Context window size (in tokens, only relevant for Ollama)
    #[arg(long, default_value = "8192")]
    num_ctx: Option<usize>,

    /// Type of tool declaration ('native' = tools via API, 'xml' = custom system message)
    #[arg(long, default_value = "xml")]
    tools_type: Option<ToolsType>,

    /// Record API responses to a file (only supported for Anthropic provider currently)
    #[arg(long)]
    record: Option<PathBuf>,

    /// Play back a recorded session from a file
    #[arg(long)]
    playback: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum Mode {
    /// Run as MCP server
    Server {
        /// Enable verbose logging
        #[arg(short, long)]
        verbose: bool,
    },
}

fn create_llm_client(
    provider: LLMProviderType,
    model: Option<String>,
    num_ctx: usize,
    record_path: Option<PathBuf>,
    playback_path: Option<PathBuf>,
) -> Result<Box<dyn LLMProvider>> {
    // If playback is specified, use the recording player regardless of provider
    if let Some(path) = playback_path {
        use crate::tests::recording_player::RecordingPlayer;
        let player = RecordingPlayer::from_file(path)?;

        if player.session_count() == 0 {
            return Err(anyhow::anyhow!("Recording file contains no sessions"));
        }

        let provider = player.create_provider()?;
        return Ok(Box::new(provider));
    }

    // Otherwise continue with normal provider setup
    if let Some(_) = record_path {
        match provider {
            LLMProviderType::Anthropic => {}
            _ => {
                eprintln!("Warning: Recording is only supported for the Anthropic provider");
            }
        }
    }
    match provider {
        LLMProviderType::Anthropic => {
            let api_key = std::env::var("ANTHROPIC_API_KEY")
                .context("ANTHROPIC_API_KEY environment variable not set")?;

            let model_name = model
                .clone()
                .unwrap_or_else(|| "claude-3-7-sonnet-20250219".to_string());

            if let Some(path) = record_path {
                Ok(Box::new(AnthropicClient::new_with_recorder(
                    api_key, model_name, path,
                )))
            } else {
                Ok(Box::new(AnthropicClient::new(api_key, model_name)))
            }
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
        // Server mode
        Some(Mode::Server { verbose }) => {
            // Setup logging based on verbose flag
            setup_logging(verbose, false);

            // Initialize server
            let mut server = MCPServer::new()?;
            server.run().await?;
        }

        // Agent mode (default)
        None => {
            // Get all the agent options from args
            let path = args.path.unwrap_or_else(|| PathBuf::from("."));
            let task = args.task;
            let continue_task = args.continue_task;
            let verbose = args.verbose;
            let provider = args.provider.unwrap_or(LLMProviderType::Anthropic);
            let model = args.model;
            let num_ctx = args.num_ctx.unwrap_or(8192);
            let tools_type = args.tools_type.unwrap_or(ToolsType::Xml);
            let use_gui = args.ui;

            // Setup logging based on verbose flag
            setup_logging(verbose, true);

            // Ensure the path exists and is a directory
            if !path.is_dir() {
                anyhow::bail!("Path '{}' is not a directory", path.display());
            }

            // Validate parameters
            if continue_task && task.is_some() {
                anyhow::bail!(
                    "Cannot specify both --task and --continue. The task will be loaded from the saved state."
                );
            }

            if !continue_task && task.is_none() && !use_gui {
                anyhow::bail!("Either --task, --continue, or --ui must be specified");
            }

            // Check if GUI mode is requested
            if use_gui {
                // Create shared state between GUI and Agent thread
                let gui = ui::gpui::GPUI::new();

                // Setup dynamic types
                let root_path = path.canonicalize()?;
                let explorer = Box::new(Explorer::new(root_path.clone()));
                let user_interface: Box<dyn UserInterface> = Box::new(gui.clone());
                let command_executor = Box::new(DefaultCommandExecutor);
                let state_persistence = Box::new(FileStatePersistence::new(root_path.clone()));

                // Start the agent in a separate thread using a standard thread
                // We need to move all the necessary components into this thread
                std::thread::spawn(move || {
                    // Create a new tokio runtime for this thread
                    let runtime = tokio::runtime::Runtime::new().unwrap();

                    // Run the agent within this runtime
                    runtime.block_on(async {
                        // Setup LLM client inside the thread
                        let llm_client = create_llm_client(
                            provider,
                            model,
                            num_ctx,
                            args.record.clone(),
                            args.playback.clone(),
                        )
                        .expect("Failed to initialize LLM client");

                        // Initialize agent
                        let mut agent = Agent::new(
                            llm_client,
                            match &tools_type {
                                ToolsType::Native => agent::ToolMode::Native,
                                ToolsType::Xml => agent::ToolMode::Xml,
                            },
                            explorer,
                            command_executor,
                            user_interface,
                            state_persistence,
                        );

                        // Get task either from state file, argument, or GUI
                        if continue_task {
                            agent.start_from_state().await.unwrap();
                        } else if let Some(task_str) = task {
                            agent.start_with_task(task_str).await.unwrap();
                        } else {
                            // In GUI mode with no task, we can start without a task
                            // The UI will prompt the user for a task
                            let task_prompt = "Please enter the task you want me to perform:";
                            let task_from_ui = agent.get_input_from_ui(task_prompt).await.unwrap();
                            agent.start_with_task(task_from_ui).await.unwrap();
                        }
                    });
                });

                // Run the GUI in the main thread - this will block until the application exits
                gui.run_app();

                // We return here when the GUI is closed
                return Ok(());
            } else {
                // Non-GUI mode - run the agent directly in the main thread
                // Setup dynamic types
                let root_path = path.canonicalize()?;
                let explorer = Box::new(Explorer::new(root_path.clone()));
                let user_interface = Box::new(TerminalUI::new());
                let command_executor = Box::new(DefaultCommandExecutor);
                let state_persistence = Box::new(FileStatePersistence::new(root_path.clone()));

                // Setup LLM client with the specified provider
                let llm_client =
                    create_llm_client(provider, model, num_ctx, args.record, args.playback)
                        .context("Failed to initialize LLM client")?;

                // Initialize agent
                let mut agent = Agent::new(
                    llm_client,
                    match &tools_type {
                        ToolsType::Native => agent::ToolMode::Native,
                        ToolsType::Xml => agent::ToolMode::Xml,
                    },
                    explorer,
                    command_executor,
                    user_interface,
                    state_persistence,
                );

                // Get task either from state file or argument
                if continue_task {
                    agent.start_from_state().await?;
                } else {
                    agent.start_with_task(task.unwrap()).await?;
                }
            }
        }
    }

    Ok(())
}
