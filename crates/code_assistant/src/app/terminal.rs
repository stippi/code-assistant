use super::AgentRunConfig;
use crate::config::DefaultProjectManager;
use crate::persistence::FileSessionPersistence;
use crate::session::manager::{AgentConfig, SessionManager};
use crate::ui::terminal::TerminalUI;
use crate::ui::{UIError, UiEvent, UserInterface};
use crate::utils::DefaultCommandExecutor;
use anyhow::{Context, Result};
use llm::factory::{create_llm_client, LLMClientConfig};
use std::sync::Arc;

pub async fn run(config: AgentRunConfig) -> Result<()> {
    // Check for experimental TUI mode
    if std::env::var("EXPERIMENTAL_TUI").is_ok() {
        let terminal_tui_app = crate::ui::terminal_tui::TerminalTuiApp::new();
        return terminal_tui_app.run(&config);
    }
    let root_path = config.path.canonicalize()?;

    // Create session persistence
    let session_persistence = FileSessionPersistence::new();

    // Setup terminal UI
    let terminal_ui = TerminalUI::new();
    let user_interface: Arc<dyn UserInterface> = Arc::new(terminal_ui.clone());

    // Setup agent configuration
    let agent_config = AgentConfig {
        tool_syntax: config.tool_syntax,
        init_path: Some(root_path.clone()),
        initial_project: root_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown")
            .to_string(),
        use_diff_blocks: config.use_diff_format,
    };

    // Create session manager
    let mut session_manager = SessionManager::new(session_persistence, agent_config);

    // Determine which session to use
    let current_session_id = if config.continue_task {
        // Try to get the latest session
        match session_manager.get_latest_session_id()? {
            Some(session_id) => {
                println!("📋 Continuing from latest session: {session_id}");
                session_id
            }
            None => {
                println!("📝 No previous session found, creating new session");
                session_manager.create_session(None)?
            }
        }
    } else {
        // Create a new session
        println!("📝 Creating new session");
        session_manager.create_session(None)?
    };

    // Load the session and display its state
    let messages = session_manager.load_session(&current_session_id)?;
    println!("📖 Loaded session with {} messages", messages.len());

    // Display welcome message and instructions
    println!();
    println!("🤖 Code Assistant Terminal UI");
    println!("Type your message to start a conversation with the AI assistant.");
    println!("Session commands (start with ':'):");
    println!("  :help      - Show help");
    println!("  :sessions  - List all sessions");
    println!("  :new [name] - Create a new session");
    println!("  :switch <id> - Switch to a session");
    println!("Type 'exit' or press Ctrl+C to quit.");
    println!();

    // Set as active session
    let ui_events = session_manager
        .set_active_session(current_session_id.clone())
        .await?;

    // Process UI events to display session state
    for event in ui_events {
        user_interface.send_event(event).await?;
    }

    // If a new task was provided, start the agent with it
    if let Some(new_task) = config.task.clone() {
        println!("🚀 Starting agent with task: {new_task}");

        // Display the user input
        user_interface
            .send_event(UiEvent::DisplayUserInput {
                content: new_task.clone(),
                attachments: Vec::new(),
            })
            .await?;

        // Start agent with the task
        start_agent_for_session(
            &mut session_manager,
            &current_session_id,
            new_task,
            &config,
            user_interface.clone(),
        )
        .await?;
    }

    // Track the current session ID for the interaction loop
    let mut current_session_id = current_session_id;

    // Main interaction loop
    loop {
        match terminal_ui.get_input().await {
            Ok(user_input) => {
                if user_input.trim().is_empty() {
                    continue; // Skip empty input
                }

                // Check for exit commands
                if user_input.trim() == "exit" || user_input.trim() == "quit" {
                    println!("👋 Goodbye!");
                    break;
                }

                // Check for session commands (starting with :)
                if user_input.starts_with(':') {
                    match handle_session_command(&user_input, &mut session_manager, &terminal_ui)
                        .await?
                    {
                        SessionCommandResult::Handled => continue,
                        SessionCommandResult::SwitchedSession(new_session_id) => {
                            current_session_id = new_session_id;
                            continue;
                        }
                        SessionCommandResult::NotHandled => {
                            // Fall through to treat as regular input
                        }
                    }
                }

                // Display the user input
                user_interface
                    .send_event(UiEvent::DisplayUserInput {
                        content: user_input.clone(),
                        attachments: Vec::new(),
                    })
                    .await?;

                // Start agent with user message
                start_agent_for_session(
                    &mut session_manager,
                    &current_session_id,
                    user_input,
                    &config,
                    user_interface.clone(),
                )
                .await?;
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

/// Result of handling a session command
enum SessionCommandResult {
    Handled,
    SwitchedSession(String),
    NotHandled,
}

/// Start an agent for a session with a user message
async fn start_agent_for_session(
    session_manager: &mut SessionManager,
    session_id: &str,
    message: String,
    config: &AgentRunConfig,
    ui: Arc<dyn UserInterface>,
) -> Result<()> {
    // Create content blocks from the message
    let content_blocks = vec![llm::ContentBlock::Text { text: message }];

    // Setup LLM client
    let llm_client = create_llm_client(LLMClientConfig {
        provider: config.provider.clone(),
        model: config.model.clone(),
        base_url: config.base_url.clone(),
        aicore_config: config.aicore_config.clone(),
        num_ctx: config.num_ctx,
        record_path: config.record.clone(),
        playback_path: config.playback.clone(),
        fast_playback: config.fast_playback,
    })
    .await
    .context("Failed to initialize LLM client")?;

    // Setup other components
    let project_manager = Box::new(DefaultProjectManager::new());
    let command_executor = Box::new(DefaultCommandExecutor);

    // Start the agent
    session_manager
        .start_agent_for_message(
            session_id,
            content_blocks,
            llm_client,
            project_manager,
            command_executor,
            ui,
        )
        .await?;

    Ok(())
}

/// Handle session management commands in terminal mode
async fn handle_session_command(
    command: &str,
    session_manager: &mut SessionManager,
    terminal_ui: &TerminalUI,
) -> Result<SessionCommandResult> {
    let parts: Vec<&str> = command[1..].split_whitespace().collect(); // Remove the ':'

    match parts.first() {
        Some(&"sessions") | Some(&"list") => {
            match session_manager.list_all_sessions() {
                Ok(sessions) => {
                    if sessions.is_empty() {
                        println!("📋 No sessions found.");
                    } else {
                        println!("📋 Available Sessions:");
                        println!("┌─────────────────────┬──────────────────────────────┬─────────┬─────────────┐");
                        println!("│ Session ID          │ Name                         │ Messages│ Last Updated│");
                        println!("├─────────────────────┼──────────────────────────────┼─────────┼─────────────┤");

                        for session in sessions {
                            let id_short = if session.id.len() > 19 {
                                format!("{}...", &session.id[..16])
                            } else {
                                format!("{:<19}", session.id)
                            };

                            let name_display = if session.name.is_empty() {
                                "(unnamed)".to_string()
                            } else if session.name.len() > 28 {
                                format!("{}...", &session.name[..25])
                            } else {
                                format!("{:<28}", session.name)
                            };

                            let updated = format_timestamp(session.updated_at);

                            println!(
                                "│ {} │ {} │ {:>7} │ {} │",
                                id_short, name_display, session.message_count, updated
                            );
                        }
                        println!("└─────────────────────┴──────────────────────────────┴─────────┴─────────────┘");
                    }
                }
                Err(e) => {
                    eprintln!("❌ Error listing sessions: {e}");
                }
            }
            Ok(SessionCommandResult::Handled)
        }
        Some(&"switch") => {
            if let Some(&partial_id) = parts.get(1) {
                // Find session by partial ID match
                match session_manager.list_all_sessions() {
                    Ok(sessions) => {
                        let matches: Vec<_> = sessions
                            .iter()
                            .filter(|s| s.id.starts_with(partial_id))
                            .collect();

                        match matches.len() {
                            0 => {
                                eprintln!("❌ No session found starting with '{partial_id}'");
                                println!("   Use :sessions to list available sessions");
                            }
                            1 => {
                                let session_id = &matches[0].id;
                                match session_manager.set_active_session(session_id.clone()).await {
                                    Ok(ui_events) => {
                                        println!("🔄 Switched to session: {session_id}");

                                        // Display session state
                                        let ui: Arc<dyn UserInterface> =
                                            Arc::new(terminal_ui.clone());
                                        for event in ui_events {
                                            let _ = ui.send_event(event).await;
                                        }

                                        return Ok(SessionCommandResult::SwitchedSession(
                                            session_id.clone(),
                                        ));
                                    }
                                    Err(e) => {
                                        eprintln!(
                                            "❌ Error switching to session {session_id}: {e}"
                                        );
                                    }
                                }
                            }
                            _ => {
                                eprintln!("❌ Multiple sessions match '{partial_id}':");
                                for session in matches {
                                    println!(
                                        "   {} - {}",
                                        session.id,
                                        if session.name.is_empty() {
                                            "(unnamed)"
                                        } else {
                                            &session.name
                                        }
                                    );
                                }
                                println!("   Please be more specific");
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("❌ Error listing sessions: {e}");
                    }
                }
            } else {
                println!("❌ Usage: :switch <session_id>");
                println!("   Use :sessions to list available sessions");
            }
            Ok(SessionCommandResult::Handled)
        }
        Some(&"new") => {
            let session_name = parts.get(1).map(|&s| s.to_string());
            match session_manager.create_session(session_name.clone()) {
                Ok(session_id) => {
                    match session_name {
                        Some(name) => println!("📝 Created new session '{name}': {session_id}"),
                        None => println!("📝 Created new session: {session_id}"),
                    }

                    // Automatically switch to the new session
                    match session_manager.set_active_session(session_id.clone()).await {
                        Ok(ui_events) => {
                            println!("🔄 Switched to new session");

                            // Display session state
                            let ui: Arc<dyn UserInterface> = Arc::new(terminal_ui.clone());
                            for event in ui_events {
                                let _ = ui.send_event(event).await;
                            }

                            return Ok(SessionCommandResult::SwitchedSession(session_id));
                        }
                        Err(e) => {
                            eprintln!("❌ Error switching to new session: {e}");
                        }
                    }
                }
                Err(e) => {
                    eprintln!("❌ Error creating session: {e}");
                }
            }
            Ok(SessionCommandResult::Handled)
        }
        Some(&"delete") | Some(&"rm") => {
            if let Some(&partial_id) = parts.get(1) {
                // Find session by partial ID match
                match session_manager.list_all_sessions() {
                    Ok(sessions) => {
                        let matches: Vec<_> = sessions
                            .iter()
                            .filter(|s| s.id.starts_with(partial_id))
                            .collect();

                        match matches.len() {
                            0 => {
                                eprintln!("❌ No session found starting with '{partial_id}'");
                                println!("   Use :sessions to list available sessions");
                            }
                            1 => {
                                let session_id = &matches[0].id;
                                let session_name = if matches[0].name.is_empty() {
                                    "(unnamed)"
                                } else {
                                    &matches[0].name
                                };

                                // Confirm deletion
                                println!(
                                    "⚠️  Are you sure you want to delete session {session_id} - {session_name}? (y/N)"
                                );
                                match terminal_ui.get_input().await {
                                    Ok(confirmation)
                                        if confirmation.trim().to_lowercase() == "y" =>
                                    {
                                        match session_manager.delete_session(session_id) {
                                            Ok(()) => {
                                                println!("🗑️  Deleted session: {session_id}");
                                            }
                                            Err(e) => {
                                                eprintln!(
                                                    "❌ Error deleting session {session_id}: {e}"
                                                );
                                            }
                                        }
                                    }
                                    _ => {
                                        println!("❌ Deletion cancelled");
                                    }
                                }
                            }
                            _ => {
                                eprintln!("❌ Multiple sessions match '{partial_id}':");
                                for session in matches {
                                    println!(
                                        "   {} - {}",
                                        session.id,
                                        if session.name.is_empty() {
                                            "(unnamed)"
                                        } else {
                                            &session.name
                                        }
                                    );
                                }
                                println!("   Please be more specific");
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("❌ Error listing sessions: {e}");
                    }
                }
            } else {
                println!("❌ Usage: :delete <session_id>");
                println!("   Use :sessions to list available sessions");
            }
            Ok(SessionCommandResult::Handled)
        }
        Some(&"help") => {
            println!("📋 Session Management Commands:");
            println!("  :sessions       - List all sessions");
            println!("  :switch <id>    - Switch to a different session");
            println!("  :new [name]     - Create a new session");
            println!("  :delete <id>    - Delete a session");
            println!("  :help           - Show this help");
            println!();
            println!("💡 Tips:");
            println!("  - Session IDs can be shortened (e.g., use first few characters)");
            println!("  - Use Ctrl+C or Ctrl+D to exit");
            Ok(SessionCommandResult::Handled)
        }
        _ => {
            // Unknown command, let it be treated as regular input
            Ok(SessionCommandResult::NotHandled)
        }
    }
}

/// Format a timestamp for display
fn format_timestamp(timestamp: std::time::SystemTime) -> String {
    use std::time::{Duration, UNIX_EPOCH};

    let duration_since_epoch = timestamp
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let now_duration = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);

    let seconds_ago = now_duration
        .as_secs()
        .saturating_sub(duration_since_epoch.as_secs());

    if seconds_ago < 60 {
        "now".to_string()
    } else if seconds_ago < 3600 {
        format!("{}m ago", seconds_ago / 60)
    } else if seconds_ago < 86400 {
        format!("{}h ago", seconds_ago / 3600)
    } else {
        format!("{}d ago", seconds_ago / 86400)
    }
}
