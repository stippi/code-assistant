mod agent;
mod config;
mod explorer;
mod mcp;
mod persistence;
mod session;
mod tools;
mod types;
mod ui;
mod utils;

#[cfg(test)]
mod tests;

use crate::agent::Agent;
use crate::mcp::MCPServer;
use crate::persistence::FileStatePersistence;
use crate::session::SessionManager;
use crate::types::ToolMode;
use crate::ui::terminal::TerminalUI;
use crate::ui::UserInterface;
use crate::utils::DefaultCommandExecutor;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use config::DefaultProjectManager;
use llm::auth::TokenManager;
use llm::config::DeploymentConfig;
use llm::{
    AiCoreClient, AnthropicClient, LLMProvider, OllamaClient, OpenAIClient, OpenRouterClient,
    VertexClient,
};
use std::io;
use std::path::PathBuf;
use tracing_subscriber::fmt::SubscriberBuilder;

#[derive(ValueEnum, Debug, Clone)]
enum LLMProviderType {
    AiCore,
    Anthropic,
    OpenAI,
    Ollama,
    Vertex,
    OpenRouter,
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

    /// Task to perform on the codebase (required in agent mode unless --continue is used, optional with --ui)
    #[arg(short, long)]
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

    /// API base URL for the LLM provider to use
    #[arg(long)]
    base_url: Option<String>,

    /// Context window size (in tokens, only relevant for Ollama)
    #[arg(long, default_value = "8192")]
    num_ctx: Option<usize>,

    /// Type of tool declaration ('native' = tools via API, 'xml' = custom system message)
    #[arg(long, default_value = "xml")]
    tools_type: Option<ToolMode>,

    /// Record API responses to a file (only supported for Anthropic provider currently)
    #[arg(long)]
    record: Option<PathBuf>,

    /// Play back a recorded session from a file
    #[arg(long)]
    playback: Option<PathBuf>,

    /// Fast playback mode - ignore chunk timing when playing recordings
    #[arg(long)]
    fast_playback: bool,

    /// Resume a specific chat session by ID
    #[arg(long)]
    chat_id: Option<String>,

    /// List available chat sessions
    #[arg(long)]
    list_chats: bool,

    /// Delete a specific chat session by ID
    #[arg(long)]
    delete_chat: Option<String>,
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

async fn create_llm_client(
    provider: LLMProviderType,
    model: Option<String>,
    base_url: Option<String>,
    num_ctx: usize,
    record_path: Option<PathBuf>,
    playback_path: Option<PathBuf>,
    fast_playback: bool,
) -> Result<Box<dyn LLMProvider>> {
    // If playback is specified, use the recording player regardless of provider
    if let Some(path) = playback_path {
        use llm::anthropic_playback::RecordingPlayer;
        let player = RecordingPlayer::from_file(path)?;

        if player.session_count() == 0 {
            return Err(anyhow::anyhow!("Recording file contains no sessions"));
        }

        let mut provider = player.create_provider()?;

        // Configure timing simulation based on command line flag
        if fast_playback {
            provider.set_simulate_timing(false);
        }

        return Ok(Box::new(provider));
    }

    // Otherwise continue with normal provider setup
    match provider {
        LLMProviderType::AiCore => {
            let config = DeploymentConfig::load()
                .context("Failed to load AiCore deployment configuration")?;
            let token_manager = TokenManager::new(&config)
                .await
                .context("Failed to initialize token manager")?;

            let base_url = base_url.unwrap_or_else(|| config.api_base_url.clone());

            if let Some(path) = record_path {
                Ok(Box::new(AiCoreClient::new_with_recorder(
                    token_manager,
                    base_url,
                    path,
                )))
            } else {
                Ok(Box::new(AiCoreClient::new(token_manager, base_url)))
            }
        }

        LLMProviderType::Anthropic => {
            let api_key = std::env::var("ANTHROPIC_API_KEY")
                .context("ANTHROPIC_API_KEY environment variable not set")?;
            let model_name = model.unwrap_or_else(|| "claude-sonnet-4-20250514".to_string());
            let base_url = base_url.unwrap_or(AnthropicClient::default_base_url());

            if let Some(path) = record_path {
                Ok(Box::new(AnthropicClient::new_with_recorder(
                    api_key, model_name, base_url, path,
                )))
            } else {
                Ok(Box::new(AnthropicClient::new(
                    api_key, model_name, base_url,
                )))
            }
        }

        LLMProviderType::OpenAI => {
            let api_key = std::env::var("OPENAI_API_KEY")
                .context("OPENAI_API_KEY environment variable not set")?;
            let model_name = model.unwrap_or_else(|| "gpt-4o".to_string());
            let base_url = base_url.unwrap_or(OpenAIClient::default_base_url());

            Ok(Box::new(OpenAIClient::new(api_key, model_name, base_url)))
        }

        LLMProviderType::Vertex => {
            let api_key = std::env::var("GOOGLE_API_KEY")
                .context("GOOGLE_API_KEY environment variable not set")?;
            let model_name = model.unwrap_or_else(|| "gemini-2.5-pro-preview-06-05".to_string());
            let base_url = base_url.unwrap_or(VertexClient::default_base_url());

            if let Some(path) = record_path {
                Ok(Box::new(VertexClient::new_with_recorder(
                    api_key,
                    model_name,
                    base_url,
                    Box::new(llm::vertex::DefaultToolIDGenerator::new()), // Add default tool_id_generator
                    path,
                )))
            } else {
                Ok(Box::new(VertexClient::new(api_key, model_name, base_url)))
            }
        }

        LLMProviderType::Ollama => {
            let base_url = base_url.unwrap_or(OllamaClient::default_base_url());

            Ok(Box::new(OllamaClient::new(
                model.context("Model name is required for Ollama provider")?,
                base_url,
                num_ctx,
            )))
        }

        LLMProviderType::OpenRouter => {
            let api_key = std::env::var("OPENROUTER_API_KEY")
                .context("OPENROUTER_API_KEY environment variable not set")?;
            let model = model.unwrap_or_else(|| "anthropic/claude-3-7-sonnet".to_string());
            let base_url = base_url.unwrap_or(OpenRouterClient::default_base_url());

            Ok(Box::new(OpenRouterClient::new(api_key, model, base_url)))
        }
    }
}

fn setup_logging(verbose: bool, use_stdout: bool) {
    let filter = {
        if verbose {
            "code_assistant=debug,llm=debug,web=debug,info".to_string()
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

async fn run_mcp_server(verbose: bool) -> Result<()> {
    // Setup logging based on verbose flag
    setup_logging(verbose, false);

    // Initialize server
    let mut server = MCPServer::new()?;
    server.run().await
}

async fn run_agent_terminal(
    path: PathBuf,
    task: Option<String>,
    session_manager: SessionManager,
    session_state: Option<crate::session::SessionState>,
    provider: LLMProviderType,
    model: Option<String>,
    base_url: Option<String>,
    num_ctx: usize,
    tools_type: ToolMode,
    record: Option<PathBuf>,
    playback: Option<PathBuf>,
    fast_playback: bool,
) -> Result<()> {
    // Non-GUI mode - run the agent directly in the main thread
    // Setup dynamic types
    let root_path = path.canonicalize()?;
    let project_manager = Box::new(DefaultProjectManager::new());
    let user_interface = Box::new(TerminalUI::new());
    let command_executor = Box::new(DefaultCommandExecutor);

    // Setup LLM client with the specified provider
    let llm_client = create_llm_client(
        provider,
        model,
        base_url,
        num_ctx,
        record,
        playback,
        fast_playback,
    )
    .await
    .context("Failed to initialize LLM client")?;

    // Initialize agent with session manager
    let mut agent = Agent::new(
        llm_client,
        tools_type,
        project_manager,
        command_executor,
        user_interface,
        session_manager,
        Some(root_path.clone()),
    );

    // Start either from session state or with new task
    if let Some(session_state) = session_state {
        agent.load_from_session_state(session_state).await
    } else {
        agent.start_with_task(task.unwrap()).await
    }
}

fn run_agent_gpui(
    path: PathBuf,
    task: Option<String>,
    session_manager: SessionManager,
    session_state: Option<crate::session::SessionState>,
    provider: LLMProviderType,
    model: Option<String>,
    base_url: Option<String>,
    num_ctx: usize,
    tools_type: ToolMode,
    record: Option<PathBuf>,
    playback: Option<PathBuf>,
    fast_playback: bool,
) -> Result<()> {
    // Create shared state between GUI and Agent thread
    let gui = ui::gpui::Gpui::new();

    // Setup chat communication channels
    let (chat_event_rx, chat_response_tx) = gui.setup_chat_communication();

    // Setup dynamic types
    let root_path = path.canonicalize()?;
    let project_manager = Box::new(DefaultProjectManager::new());
    let user_interface: Box<dyn UserInterface> = Box::new(gui.clone());
    let command_executor = Box::new(DefaultCommandExecutor);

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
                base_url,
                num_ctx,
                record,
                playback,
                fast_playback,
            )
            .await
            .expect("Failed to initialize LLM client");

            // Initialize agent with session manager
            let mut agent = Agent::new(
                llm_client,
                tools_type,
                project_manager,
                command_executor,
                user_interface,
                session_manager,
                Some(root_path.clone()),
            );

            // Clone necessary data for chat management task
            let chat_event_rx_clone = chat_event_rx.clone();
            let chat_response_tx_clone = chat_response_tx.clone();

            // Spawn task to handle chat management events
            // Keep the task handle to prevent it from being dropped
            let _chat_management_task = tokio::spawn(async move {
                tracing::info!("Chat management task started");
                while let Ok(event) = chat_event_rx_clone.recv().await {
                    tracing::info!("Chat management event received: {:?}", event);
                    let response = match event {
                        ui::gpui::ChatManagementEvent::ListSessions => {
                            // Create a new session manager for this operation
                            let persistence =
                                crate::persistence::FileStatePersistence::new(root_path.clone());
                            let session_manager = crate::session::SessionManager::new(persistence);
                            match session_manager.list_sessions() {
                                Ok(sessions) => {
                                    ui::gpui::ChatManagementResponse::SessionsListed { sessions }
                                }
                                Err(e) => ui::gpui::ChatManagementResponse::Error {
                                    message: e.to_string(),
                                },
                            }
                        }
                        ui::gpui::ChatManagementEvent::LoadSession { session_id } => {
                            // Load session and send fragments directly to UI
                            let persistence = crate::persistence::FileStatePersistence::new(root_path.clone());
                            let mut session_manager = crate::session::SessionManager::new(persistence);

                            match session_manager.load_session(&session_id) {
                                Ok(session_state) => {
                                    tracing::info!("Loaded session {} with {} messages", session_id, session_state.messages.len());

                                    ui::gpui::ChatManagementResponse::SessionLoaded {
                                        session_id,
                                        messages: session_state.messages
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("Failed to load session {}: {}", session_id, e);
                                    ui::gpui::ChatManagementResponse::Error {
                                        message: format!("Failed to load session: {}", e),
                                    }
                                }
                            }
                        }
                        ui::gpui::ChatManagementEvent::CreateNewSession { name } => {
                            let persistence =
                                crate::persistence::FileStatePersistence::new(root_path.clone());
                            let mut session_manager =
                                crate::session::SessionManager::new(persistence);
                            match session_manager.create_session(name) {
                                Ok(session_id) => {
                                    let display_name = format!("Chat {}", &session_id[5..13]);
                                    ui::gpui::ChatManagementResponse::SessionCreated {
                                        session_id,
                                        name: display_name,
                                    }
                                }
                                Err(e) => ui::gpui::ChatManagementResponse::Error {
                                    message: e.to_string(),
                                },
                            }
                        }
                        ui::gpui::ChatManagementEvent::DeleteSession { session_id } => {
                            let persistence =
                                crate::persistence::FileStatePersistence::new(root_path.clone());
                            let mut session_manager =
                                crate::session::SessionManager::new(persistence);
                            match session_manager.delete_session(&session_id) {
                                Ok(_) => {
                                    ui::gpui::ChatManagementResponse::SessionDeleted { session_id }
                                }
                                Err(e) => ui::gpui::ChatManagementResponse::Error {
                                    message: e.to_string(),
                                },
                            }
                        }
                    };

                    tracing::info!("Sending chat management response: {:?}", response);
                    let _ = chat_response_tx_clone.send(response).await;
                }
            });

            // Start either from session state, task, or GUI input
            if let Some(session_state) = session_state {
                agent.load_from_session_state(session_state).await.unwrap();
            } else if let Some(task_str) = task {
                agent.start_with_task(task_str).await.unwrap();
            } else {
                // In GUI mode with no task, prompt the user for a task
                let task_from_ui = agent.get_input_from_ui().await.unwrap();
                agent.start_with_task(task_from_ui).await.unwrap();
            }
        });
    });

    // Run the GUI in the main thread - this will block until the application exits
    gui.run_app();

    // We return here when the GUI is closed
    Ok(())
}

async fn run_agent(args: Args) -> Result<()> {
    // Get all the agent options from args
    let path = args.path.clone().unwrap_or_else(|| PathBuf::from("."));
    let task = args.task.clone();
    let continue_task = args.continue_task;
    let verbose = args.verbose;
    let provider = args.provider.unwrap_or(LLMProviderType::Anthropic);
    let model = args.model.clone();
    let base_url = args.base_url.clone();
    let num_ctx = args.num_ctx.unwrap_or(8192);
    let tools_type = args.tools_type.unwrap_or(ToolMode::Xml);
    let use_gui = args.ui;

    // Setup logging based on verbose flag
    setup_logging(verbose, true);

    // Ensure the path exists and is a directory
    if !path.is_dir() {
        anyhow::bail!("Path '{}' is not a directory", path.display());
    }

    // Create session manager for chat functionality
    let persistence = FileStatePersistence::new(path.clone());
    let mut session_manager = SessionManager::new(persistence);

    // Handle chat session logic
    let (session_task, session_state) = if let Some(chat_id) = args.chat_id {
        // Load specific chat session
        let session_state = session_manager.load_session(&chat_id)?;
        println!("Loaded chat session: {}", chat_id);
        (None, Some(session_state))
    } else if continue_task {
        // Try to continue from latest session
        if let Some(latest_id) = session_manager.get_latest_session_id()? {
            let session_state = session_manager.load_session(&latest_id)?;
            println!("Continuing latest chat session: {}", latest_id);
            (None, Some(session_state))
        } else {
            anyhow::bail!("No chat sessions found to continue from. Please start with a task.");
        }
    } else {
        // Create new session with task
        if task.is_some() {
            let new_session_id = session_manager.create_session(None)?;
            println!("Created new chat session: {}", new_session_id);
            (task, None)
        } else {
            anyhow::bail!("Please provide a task to start a new chat session.");
        }
    };

    // Run in either GUI or terminal mode
    if use_gui {
        run_agent_gpui(
            path,
            session_task,
            session_manager,
            session_state,
            provider,
            model,
            base_url,
            num_ctx,
            tools_type,
            args.record.clone(),
            args.playback.clone(),
            args.fast_playback,
        )
    } else {
        run_agent_terminal(
            path,
            session_task,
            session_manager,
            session_state,
            provider,
            model,
            base_url,
            num_ctx,
            tools_type,
            args.record.clone(),
            args.playback.clone(),
            args.fast_playback,
        )
        .await
    }
}

/// List all available chat sessions
async fn handle_list_chats(root_path: &PathBuf) -> Result<()> {
    let persistence = FileStatePersistence::new(root_path.clone());
    let session_manager = SessionManager::new(persistence);

    let sessions = session_manager.list_sessions()?;
    if sessions.is_empty() {
        println!("No chat sessions found.");
    } else {
        println!("Available chat sessions:");
        for session in sessions {
            println!(
                "  {} - {} ({} messages, created {})",
                session.id,
                session.name,
                session.message_count,
                crate::persistence::format_time(session.created_at)
            );
        }
    }
    Ok(())
}

/// Delete a specific chat session
async fn handle_delete_chat(root_path: &PathBuf, session_id: &str) -> Result<()> {
    let persistence = FileStatePersistence::new(root_path.clone());
    let mut session_manager = SessionManager::new(persistence);

    // Check if session exists first
    let sessions = session_manager.list_sessions()?;
    if !sessions.iter().any(|s| s.id == session_id) {
        anyhow::bail!("Chat session '{}' not found", session_id);
    }

    session_manager.delete_session(session_id)?;
    println!("Chat session '{}' deleted successfully.", session_id);
    Ok(())
}

/// Handle chat-related command line operations
async fn handle_chat_commands(args: &Args) -> Result<bool> {
    let default_path = PathBuf::from(".");
    let root_path = args.path.as_ref().unwrap_or(&default_path);

    // Handle list chats command
    if args.list_chats {
        handle_list_chats(root_path).await?;
        return Ok(true);
    }

    // Handle delete chat command
    if let Some(session_id) = &args.delete_chat {
        handle_delete_chat(root_path, session_id).await?;
        return Ok(true);
    }

    // Other chat commands would be handled in main function
    Ok(false)
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse command line arguments
    let args = Args::parse();

    // Handle chat-related commands first
    if handle_chat_commands(&args).await? {
        return Ok(()); // Chat command was handled, exit
    }

    match args.mode {
        // Server mode
        Some(Mode::Server { verbose }) => run_mcp_server(verbose).await,

        // Agent mode (default)
        None => run_agent(args).await,
    }
}
