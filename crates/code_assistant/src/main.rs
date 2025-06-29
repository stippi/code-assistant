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

use crate::agent::{Agent, FileStatePersistence};
use crate::mcp::MCPServer;
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
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info, trace};
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

    /// Task to perform on the codebase (required in terminal mode, optional with --ui)
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
    continue_task: bool,
    provider: LLMProviderType,
    model: Option<String>,
    base_url: Option<String>,
    num_ctx: usize,
    tools_type: ToolMode,
    record: Option<PathBuf>,
    playback: Option<PathBuf>,
    fast_playback: bool,
) -> Result<()> {
    let root_path = path.canonicalize()?;

    // Create file persistence for simple state management
    let file_persistence = FileStatePersistence::new(&root_path, tools_type);

    // Setup dynamic types
    let project_manager = Box::new(DefaultProjectManager::new());
    let user_interface = Arc::new(Box::new(TerminalUI::new()) as Box<dyn UserInterface>);
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

    // Create agent with file persistence
    let state_storage = Box::new(file_persistence.clone());
    let mut agent = Agent::new(
        llm_client,
        tools_type,
        project_manager,
        command_executor,
        user_interface,
        state_storage,
        Some(root_path.clone()),
    );

    // Check if we should continue from previous state or start new
    if continue_task && file_persistence.has_saved_state() {
        // Load from saved state
        if let Some(saved_session) = file_persistence.load_agent_state()? {
            println!(
                "Continuing from previous state with {} messages",
                saved_session.messages.len()
            );

            // Convert ChatSession to SessionState for the agent
            let session_state = crate::session::SessionState {
                messages: saved_session.messages,
                tool_executions: saved_session
                    .tool_executions
                    .iter()
                    .map(|se| se.deserialize())
                    .collect::<Result<Vec<_>>>()?,
                working_memory: saved_session.working_memory,
                init_path: saved_session.init_path,
                initial_project: saved_session.initial_project,
                next_request_id: Some(saved_session.next_request_id),
            };

            agent.load_from_session_state(session_state).await?;
        } else {
            agent.init_working_memory()?;
        }
    } else {
        agent.init_working_memory()?;
    }

    // If a new task was provided, add it and continue
    if let Some(new_task) = task {
        println!("Adding new task: {}", new_task);
        let user_msg = llm::Message {
            role: llm::MessageRole::User,
            content: llm::MessageContent::Text(new_task),
            request_id: None,
            usage: None,
        };
        agent.append_message(user_msg)?;
    }

    agent.run_agent_loop().await
}

fn run_agent_gpui(
    path: PathBuf,
    task: Option<String>,
    provider: LLMProviderType,
    model: Option<String>,
    base_url: Option<String>,
    num_ctx: usize,
    tools_type: ToolMode,
    record: Option<PathBuf>,
    playback: Option<PathBuf>,
    fast_playback: bool,
) -> Result<()> {
    use crate::session::{AgentConfig, SessionManager};

    // Create shared state between GUI and backend
    let gui = ui::gpui::Gpui::new();

    // Setup unified backend communication
    let (backend_event_rx, backend_response_tx) = gui.setup_backend_communication();

    // Setup dynamic types for MultiSessionManager
    let root_path = path.canonicalize()?;
    let persistence = crate::persistence::FileSessionPersistence::new();

    let agent_config = AgentConfig {
        tool_mode: tools_type,
        init_path: Some(root_path.clone()),
        initial_project: None,
    };

    // Create the new SessionManager
    let multi_session_manager =
        Arc::new(Mutex::new(SessionManager::new(persistence, agent_config)));

    // Clone GUI before moving it into thread
    let gui_for_thread = gui.clone();
    let task_clone = task.clone();

    // Start the simplified backend thread
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();

        runtime.block_on(async {
            if let Some(initial_task) = task_clone {
                // Task provided - create new session and start agent
                debug!("Creating initial session with task: {}", initial_task);

                let session_id = {
                    let mut manager = multi_session_manager.lock().unwrap();
                    manager.create_session(None).unwrap()
                };

                debug!("Created initial session: {}", session_id);

                // Connect session to UI and start agent
                let ui_events = {
                    let mut manager = multi_session_manager.lock().unwrap();
                    manager
                        .set_active_session(session_id.clone())
                        .await
                        .unwrap_or_else(|e| {
                            error!("Failed to set active session: {}", e);
                            Vec::new()
                        })
                };

                for event in ui_events {
                    if let Err(e) = gui_for_thread.send_event(event).await {
                        error!("Failed to send UI event: {}", e);
                    }
                }

                let project_manager = Box::new(DefaultProjectManager::new());
                let command_executor = Box::new(DefaultCommandExecutor);
                let user_interface =
                    Arc::new(Box::new(gui_for_thread.clone()) as Box<dyn UserInterface>);

                let llm_client = create_llm_client(
                    provider.clone(),
                    model.clone(),
                    base_url.clone(),
                    num_ctx,
                    record.clone(),
                    playback.clone(),
                    fast_playback,
                )
                .await
                .expect("Failed to create LLM client");

                {
                    let mut manager = multi_session_manager.lock().unwrap();
                    manager
                        .start_agent_for_message(
                            &session_id,
                            initial_task,
                            llm_client,
                            project_manager,
                            command_executor,
                            user_interface,
                        )
                        .await
                        .expect("Failed to start agent with initial task");
                }

                debug!("Started agent for initial session");
            } else {
                // No task - connect to latest existing session
                info!("No task provided, connecting to latest session");

                let latest_session_id = {
                    let manager = multi_session_manager.lock().unwrap();
                    manager.get_latest_session_id().unwrap_or(None)
                };

                if let Some(session_id) = latest_session_id {
                    debug!("Connecting to existing session: {}", session_id);

                    let ui_events = {
                        let mut manager = multi_session_manager.lock().unwrap();
                        manager
                            .set_active_session(session_id.clone())
                            .await
                            .unwrap_or_else(|e| {
                                error!("Failed to set active session: {}", e);
                                Vec::new()
                            })
                    };

                    for event in ui_events {
                        if let Err(e) = gui_for_thread.send_event(event).await {
                            error!("Failed to send UI event: {}", e);
                        }
                    }
                } else {
                    info!("No existing sessions found - UI will start empty");
                }
            }

            handle_backend_events(
                backend_event_rx,
                backend_response_tx,
                multi_session_manager,
                provider,
                model,
                base_url,
                num_ctx,
                record,
                playback,
                fast_playback,
                gui_for_thread,
            )
            .await;
        });
    });

    // Run the GUI in the main thread
    gui.run_app();

    Ok(())
}

async fn run_agent(args: Args) -> Result<()> {
    // Get all the agent options from args
    let path = args.path.clone().unwrap_or_else(|| PathBuf::from("."));
    let task = args.task.clone();
    let _continue_task = args.continue_task;
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

    // Run in either GUI or terminal mode
    if use_gui {
        run_agent_gpui(
            path.clone(),
            task, // Can be None - will connect to latest session instead
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
        // Terminal mode
        run_agent_terminal(
            path,
            task, // Can be None - will prompt user then
            args.continue_task,
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

/// Simplified backend event handler that fixes mutex/await boundary issues
async fn handle_backend_events(
    backend_event_rx: async_channel::Receiver<ui::gpui::BackendEvent>,
    backend_response_tx: async_channel::Sender<ui::gpui::BackendResponse>,
    multi_session_manager: Arc<Mutex<crate::session::SessionManager>>,
    provider: LLMProviderType,
    model: Option<String>,
    base_url: Option<String>,
    num_ctx: usize,
    record: Option<PathBuf>,
    playback: Option<PathBuf>,
    fast_playback: bool,
    gui: ui::gpui::Gpui,
) {
    debug!("Backend event handler started");

    while let Ok(event) = backend_event_rx.recv().await {
        debug!("Backend event: {:?}", event);

        let response = match event {
            ui::gpui::BackendEvent::ListSessions => {
                // Simple sync operation - no mutex across await
                let sessions = {
                    let manager = multi_session_manager.lock().unwrap();
                    manager.list_all_sessions()
                };
                match sessions {
                    Ok(sessions) => {
                        trace!("Found {} sessions", sessions.len());
                        ui::gpui::BackendResponse::SessionsListed { sessions }
                    }
                    Err(e) => {
                        error!("Failed to list sessions: {}", e);
                        ui::gpui::BackendResponse::Error {
                            message: e.to_string(),
                        }
                    }
                }
            }

            ui::gpui::BackendEvent::CreateNewSession { name } => {
                // Clone the manager reference for the async call
                let manager_clone = multi_session_manager.clone();
                let create_result = {
                    let mut manager = manager_clone.lock().unwrap();
                    manager.create_session(name.clone())
                };

                // Now handle result without any locks
                match create_result {
                    Ok(session_id) => {
                        info!("Created session {}", session_id);
                        ui::gpui::BackendResponse::SessionCreated { session_id }
                    }
                    Err(e) => {
                        error!("Failed to create session: {}", e);
                        ui::gpui::BackendResponse::Error {
                            message: e.to_string(),
                        }
                    }
                }
            }

            ui::gpui::BackendEvent::LoadSession { session_id } => {
                debug!("LoadSession requested: {}", session_id);

                // Use set_active_session to get properly processed UI events (same as initial connection)
                let ui_events_result = {
                    let mut manager = multi_session_manager.lock().unwrap();
                    manager.set_active_session(session_id.clone()).await
                };

                // Handle result and send UI events directly
                match ui_events_result {
                    Ok(ui_events) => {
                        trace!("Session connected with {} UI events", ui_events.len());

                        // Send all UI events to update the interface
                        for event in ui_events {
                            if let Err(e) = gui.send_event(event).await {
                                error!("Failed to send UI event: {}", e);
                            }
                        }

                        // DON'T return a response - UI events already handled the update
                        // This prevents the duplicate message processing in handle_backend_response
                        continue;
                    }
                    Err(e) => {
                        error!("Failed to connect to session {}: {}", session_id, e);
                        ui::gpui::BackendResponse::Error {
                            message: e.to_string(),
                        }
                    }
                }
            }

            ui::gpui::BackendEvent::DeleteSession { session_id } => {
                debug!("DeleteSession requested: {}", session_id);

                // Now we can call delete_session directly since it's synchronous
                let delete_result = {
                    let mut manager = multi_session_manager.lock().unwrap();
                    manager.delete_session(&session_id)
                };

                match delete_result {
                    Ok(_) => {
                        debug!("Session deleted: {}", session_id);
                        ui::gpui::BackendResponse::SessionDeleted { session_id }
                    }
                    Err(e) => {
                        error!("Failed to delete session {}: {}", session_id, e);
                        ui::gpui::BackendResponse::Error {
                            message: e.to_string(),
                        }
                    }
                }
            }

            ui::gpui::BackendEvent::SendUserMessage {
                session_id,
                message,
            } => {
                debug!("User message for session {}: {}", session_id, message);

                // First: Display the user message immediately in the UI
                if let Err(e) = gui
                    .send_event(crate::ui::UiEvent::DisplayUserInput {
                        content: message.clone(),
                    })
                    .await
                {
                    error!("Failed to display user message: {}", e);
                }

                // Use MultiSessionManager.start_agent_for_message() instead of creating a separate agent
                let result = {
                    // Create components for the agent
                    let project_manager = Box::new(DefaultProjectManager::new());
                    let command_executor = Box::new(DefaultCommandExecutor);
                    let user_interface = Arc::new(Box::new(gui.clone()) as Box<dyn UserInterface>);

                    // Create LLM client
                    let llm_client = create_llm_client(
                        provider.clone(),
                        model.clone(),
                        base_url.clone(),
                        num_ctx,
                        record.clone(),
                        playback.clone(),
                        fast_playback,
                    )
                    .await;

                    match llm_client {
                        Ok(client) => {
                            // Use the MultiSessionManager to start the agent properly
                            let mut manager = multi_session_manager.lock().unwrap();
                            manager
                                .start_agent_for_message(
                                    &session_id,
                                    message.clone(),
                                    client,
                                    project_manager,
                                    command_executor,
                                    user_interface,
                                )
                                .await
                        }
                        Err(e) => {
                            error!("Failed to create LLM client: {}", e);
                            Err(e)
                        }
                    }
                };

                match result {
                    Ok(_) => {
                        debug!("Agent started for session {}", session_id);
                        // No response needed for SendUserMessage - agent communicates via UI
                        continue;
                    }
                    Err(e) => {
                        error!("Failed to start agent for session {}: {}", session_id, e);
                        ui::gpui::BackendResponse::Error {
                            message: format!("Failed to start agent: {}", e),
                        }
                    }
                }
            }
        };

        // Send response back to UI
        if let Err(e) = backend_response_tx.send(response).await {
            error!("Failed to send response: {}", e);
            break;
        }
    }

    debug!("Backend event handler stopped");
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse command line arguments
    let args = Args::parse();

    match args.mode {
        // Server mode
        Some(Mode::Server { verbose }) => run_mcp_server(verbose).await,

        // Agent mode (default)
        None => run_agent(args).await,
    }
}
