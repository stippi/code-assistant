use crate::agent::{Agent, FileStatePersistence};
use crate::types::ToolSyntax;
use crate::ui::terminal::TerminalUI;
use crate::ui::{UIError, UiEvent, UserInterface};
use crate::utils::DefaultCommandExecutor;
use anyhow::{Context, Result};
use crate::config::DefaultProjectManager;
use llm::factory::{LLMClientConfig, LLMProviderType, create_llm_client};
use std::path::PathBuf;
use std::sync::Arc;

pub async fn run(
    path: PathBuf,
    task: Option<String>,
    continue_task: bool,
    provider: LLMProviderType,
    model: Option<String>,
    base_url: Option<String>,
    aicore_config: Option<PathBuf>,
    num_ctx: usize,
    tool_syntax: ToolSyntax,
    use_diff_format: bool,
    record: Option<PathBuf>,
    playback: Option<PathBuf>,
    fast_playback: bool,
) -> Result<()> {
    let root_path = path.canonicalize()?;

    // Create file persistence for simple state management
    let file_persistence = FileStatePersistence::new(&root_path, tool_syntax, use_diff_format);

    // Setup dynamic types
    let project_manager = Box::new(DefaultProjectManager::new());
    let terminal_ui = TerminalUI::new();
    let user_interface: Arc<dyn UserInterface> = Arc::new(terminal_ui.clone());
    let command_executor = Box::new(DefaultCommandExecutor);

    // Setup LLM client with the specified provider
    let llm_client = create_llm_client(LLMClientConfig {
        provider,
        model,
        base_url,
        aicore_config,
        num_ctx,
        record_path: record,
        playback_path: playback,
        fast_playback,
    })
    .await
    .context("Failed to initialize LLM client")?;

    // Create agent with file persistence
    let state_storage = Box::new(file_persistence.clone());
    let mut agent = Agent::new(
        llm_client,
        tool_syntax,
        project_manager,
        command_executor,
        user_interface.clone(),
        state_storage,
        Some(root_path.clone()),
    );

    // Configure diff blocks format if requested
    if use_diff_format {
        agent.enable_diff_blocks();
    }

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
                session_id: saved_session.id.clone(),
                name: String::new(),
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
        println!("Adding new task: {new_task}");
        let user_msg = llm::Message {
            role: llm::MessageRole::User,
            content: llm::MessageContent::Text(new_task.clone()),
            request_id: None,
            usage: None,
        };
        agent.append_message(user_msg)?;

        // Display the user input in the terminal
        user_interface
            .send_event(UiEvent::DisplayUserInput {
                content: new_task,
                attachments: Vec::new(),
            })
            .await?;
    }

    // Run the agent using single iterations and handle user input externally
    loop {
        // Run a single iteration
        agent.run_single_iteration().await?;

        // Check if we need user input by trying to get it
        // The terminal UI will block until user provides input
        match terminal_ui.get_input().await {
            Ok(user_input) => {
                if user_input.trim().is_empty() {
                    continue; // Skip empty input
                }

                // Display the user input
                user_interface
                    .send_event(UiEvent::DisplayUserInput {
                        content: user_input.clone(),
                        attachments: Vec::new(),
                    })
                    .await?;

                // Add user message to agent
                let user_msg = llm::Message {
                    role: llm::MessageRole::User,
                    content: llm::MessageContent::Text(user_input),
                    request_id: None,
                    usage: None,
                };
                agent.append_message(user_msg)?;
            }
            Err(UIError::IOError(e)) if e.kind() == std::io::ErrorKind::Interrupted => {
                // Ctrl-C
                println!("\nExiting...");
                break;
            }
            Err(UIError::IOError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                // Ctrl-D
                println!("\nExiting...");
                break;
            }
            Err(e) => {
                eprintln!("Error getting user input: {e}");
                break;
            }
        }
    }

    Ok(())
}
