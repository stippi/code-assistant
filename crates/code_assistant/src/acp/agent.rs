use agent_client_protocol as acp;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::acp::types::convert_prompt_to_content_blocks;
use crate::acp::ACPUserUI;
use crate::config::DefaultProjectManager;
use crate::persistence::LlmSessionConfig;
use crate::session::instance::SessionActivityState;
use crate::session::{AgentConfig, AgentLaunchResources, SessionManager};
use crate::ui::UserInterface;
use crate::utils::DefaultCommandExecutor;
use llm::factory::{create_llm_client, LLMClientConfig};

/// Global connection to the ACP client
/// Since there's only one connection per agent process, this is acceptable
static ACP_CLIENT_CONNECTION: OnceLock<Arc<acp::AgentSideConnection>> = OnceLock::new();

/// Set the global ACP client connection
pub fn set_acp_client_connection(connection: Arc<acp::AgentSideConnection>) {
    if ACP_CLIENT_CONNECTION.set(connection).is_err() {
        tracing::warn!("ACP client connection was already set");
    }
}

/// Get the global ACP client connection
pub fn get_acp_client_connection() -> Option<Arc<acp::AgentSideConnection>> {
    ACP_CLIENT_CONNECTION.get().cloned()
}

pub struct ACPAgentImpl {
    session_manager: Arc<Mutex<SessionManager>>,
    agent_config: AgentConfig,
    llm_config: LLMClientConfig,
    session_update_tx: mpsc::UnboundedSender<(acp::SessionNotification, oneshot::Sender<()>)>,
    /// Active UI instances for running prompts, keyed by session ID
    /// Used to signal cancellation to the prompt() wait loop
    active_uis: Arc<Mutex<HashMap<String, Arc<ACPUserUI>>>>,
    client_capabilities: Arc<Mutex<Option<acp::ClientCapabilities>>>,
}

impl ACPAgentImpl {
    pub fn new(
        session_manager: Arc<Mutex<SessionManager>>,
        agent_config: AgentConfig,
        llm_config: LLMClientConfig,
        session_update_tx: mpsc::UnboundedSender<(acp::SessionNotification, oneshot::Sender<()>)>,
    ) -> Self {
        Self {
            session_manager,
            agent_config,
            llm_config,
            session_update_tx,
            active_uis: Arc::new(Mutex::new(HashMap::new())),
            client_capabilities: Arc::new(Mutex::new(None)),
        }
    }
}

impl acp::Agent for ACPAgentImpl {
    #[allow(clippy::manual_async_fn)]
    fn initialize<'life0, 'async_trait>(
        &'life0 self,
        arguments: acp::InitializeRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<acp::InitializeResponse, acp::Error>>
                + 'async_trait,
        >,
    >
    where
        Self: 'async_trait,
        'life0: 'async_trait,
    {
        let client_capabilities = self.client_capabilities.clone();

        Box::pin(async move {
            tracing::info!("ACP: Received initialize request");

            {
                let mut caps = client_capabilities.lock().await;
                *caps = Some(arguments.client_capabilities.clone());
            }

            Ok(acp::InitializeResponse {
                protocol_version: acp::V1,
                agent_capabilities: acp::AgentCapabilities {
                    load_session: true,
                    mcp_capabilities: acp::McpCapabilities {
                        http: false,
                        sse: false,
                        meta: None,
                    },
                    prompt_capabilities: acp::PromptCapabilities {
                        image: true,
                        audio: false,
                        embedded_context: true,
                        meta: None,
                    },
                    meta: None,
                },
                auth_methods: Vec::new(),
                meta: None,
            })
        })
    }

    #[allow(clippy::manual_async_fn)]
    fn authenticate<'life0, 'async_trait>(
        &'life0 self,
        _arguments: acp::AuthenticateRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<acp::AuthenticateResponse, acp::Error>>
                + 'async_trait,
        >,
    >
    where
        Self: 'async_trait,
        'life0: 'async_trait,
    {
        Box::pin(async move {
            tracing::info!("ACP: Received authenticate request");
            Ok(acp::AuthenticateResponse { meta: None })
        })
    }

    fn new_session<'life0, 'async_trait>(
        &'life0 self,
        arguments: acp::NewSessionRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<acp::NewSessionResponse, acp::Error>>
                + 'async_trait,
        >,
    >
    where
        Self: 'async_trait,
        'life0: 'async_trait,
    {
        let session_manager = self.session_manager.clone();
        let llm_config = self.llm_config.clone();
        let agent_config = self.agent_config.clone();

        Box::pin(async move {
            tracing::info!("ACP: Creating new session with cwd: {:?}", arguments.cwd);

            // Update the agent config to use the provided cwd
            let mut _session_agent_config = agent_config;
            _session_agent_config.session_config.init_path = Some(arguments.cwd);

            let llm_session_config = LlmSessionConfig {
                provider: llm_config.provider,
                model: llm_config.model,
                base_url: llm_config.base_url,
                aicore_config: llm_config.aicore_config,
                num_ctx: llm_config.num_ctx,
                record_path: llm_config.record_path,
            };

            let session_id = {
                let mut manager = session_manager.lock().await;
                // Update the manager's agent config for this session
                // Actually, we need to pass this differently...
                // For now, let's just use it when we start the agent
                manager
                    .create_session_with_config(None, Some(llm_session_config))
                    .map_err(|e| {
                        tracing::error!("Failed to create session: {}", e);
                        acp::Error::internal_error()
                    })?
            };

            tracing::info!("ACP: Created session: {}", session_id);

            Ok(acp::NewSessionResponse {
                session_id: acp::SessionId(session_id.into()),
                modes: None, // TODO: Support modes like "Plan", "Architect" and "Code".
                meta: None,
            })
        })
    }

    fn load_session<'life0, 'async_trait>(
        &'life0 self,
        arguments: acp::LoadSessionRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<acp::LoadSessionResponse, acp::Error>>
                + 'async_trait,
        >,
    >
    where
        Self: 'async_trait,
        'life0: 'async_trait,
    {
        let session_manager = self.session_manager.clone();
        let session_update_tx = self.session_update_tx.clone();

        Box::pin(async move {
            tracing::info!("ACP: Loading session: {}", arguments.session_id.0);

            // Load session into manager
            {
                let mut manager = session_manager.lock().await;
                manager.load_session(&arguments.session_id.0).map_err(|e| {
                    tracing::error!("Failed to load session: {}", e);
                    acp::Error::internal_error()
                })?;
            }

            // Replay message history as session/update events
            // We need to reconstruct the replay logic here since we moved self fields
            let (tool_syntax, messages, base_path) = {
                let manager = session_manager.lock().await;
                let session_instance = manager
                    .get_session(&arguments.session_id.0)
                    .ok_or_else(acp::Error::internal_error)?;

                (
                    session_instance.session.config.tool_syntax,
                    session_instance.session.messages.clone(),
                    session_instance.session.config.init_path.clone(),
                )
            };

            // Create a UI for this session
            let ui = Arc::new(ACPUserUI::new(
                arguments.session_id.clone(),
                session_update_tx,
                base_path,
            ));

            // Create stream processor to extract fragments
            let mut processor =
                crate::ui::streaming::create_stream_processor(tool_syntax, ui.clone(), 0);

            // Process each message to extract and send fragments
            for message in messages {
                let fragments = processor
                    .extract_fragments_from_message(&message)
                    .map_err(|_| acp::Error::internal_error())?;

                for fragment in fragments {
                    ui.display_fragment(&fragment)
                        .map_err(|_| acp::Error::internal_error())?;
                }
            }

            tracing::info!("ACP: Loaded session: {}", arguments.session_id.0);

            Ok(acp::LoadSessionResponse {
                modes: None,
                meta: None,
            })
        })
    }

    fn prompt<'life0, 'async_trait>(
        &'life0 self,
        arguments: acp::PromptRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<acp::PromptResponse, acp::Error>>
                + 'async_trait,
        >,
    >
    where
        Self: 'async_trait,
        'life0: 'async_trait,
    {
        let session_manager = self.session_manager.clone();
        let session_update_tx = self.session_update_tx.clone();
        let llm_config = self.llm_config.clone();
        let active_uis = self.active_uis.clone();
        let client_capabilities = self.client_capabilities.clone();

        Box::pin(async move {
            tracing::info!(
                "ACP: Received prompt for session: {}",
                arguments.session_id.0
            );

            let terminal_supported = {
                let caps = client_capabilities.lock().await;
                caps.as_ref().map(|caps| caps.terminal).unwrap_or(false)
            };

            let base_path = {
                let manager = session_manager.lock().await;
                manager
                    .get_session(&arguments.session_id.0)
                    .and_then(|session| session.session.config.init_path.clone())
            };

            // Create UI for this session
            let acp_ui = Arc::new(ACPUserUI::new(
                arguments.session_id.clone(),
                session_update_tx.clone(),
                base_path,
            ));

            // Store it so cancel() can reach it
            {
                let mut uis = active_uis.lock().await;
                uis.insert(arguments.session_id.0.to_string(), acp_ui.clone());
            }

            let ui: Arc<dyn crate::ui::UserInterface> = acp_ui.clone();

            // Clone for error closure
            let error_session_id = arguments.session_id.clone();
            let error_tx = session_update_tx.clone();

            // Helper to send error messages to client
            let send_error = |error_msg: String| async move {
                let (tx, _rx) = oneshot::channel();
                let _ = error_tx.send((
                    acp::SessionNotification {
                        session_id: error_session_id.clone(),
                        update: acp::SessionUpdate::AgentMessageChunk {
                            content: acp::ContentBlock::Text(acp::TextContent {
                                annotations: None,
                                text: format!("ERROR: {error_msg}"),
                                meta: None,
                            }),
                        },
                        meta: None,
                    },
                    tx,
                ));
            };

            // Convert prompt content blocks
            let content_blocks = convert_prompt_to_content_blocks(arguments.prompt);

            let session_llm_config = LlmSessionConfig {
                provider: llm_config.provider.clone(),
                model: llm_config.model.clone(),
                base_url: llm_config.base_url.clone(),
                aicore_config: llm_config.aicore_config.clone(),
                num_ctx: llm_config.num_ctx,
                record_path: llm_config.record_path.clone(),
            };

            // Create LLM client
            let llm_client = match create_llm_client(llm_config).await {
                Ok(client) => client,
                Err(e) => {
                    let error_msg = format!("Failed to create LLM client: {e}");
                    tracing::error!("{}", error_msg);
                    send_error(error_msg).await;
                    let mut uis = active_uis.lock().await;
                    uis.remove(arguments.session_id.0.as_ref());
                    return Ok(acp::PromptResponse {
                        stop_reason: acp::StopReason::EndTurn,
                        meta: None,
                    });
                }
            };

            // Create project manager and command executor
            let project_manager = Box::new(DefaultProjectManager::new());

            // Use ACP Terminal Command Executor if client connection is available
            let command_executor: Box<dyn crate::utils::command::CommandExecutor> = {
                if terminal_supported && get_acp_client_connection().is_some() {
                    tracing::info!(
                        "ACP: Using ACPTerminalCommandExecutor for session {}",
                        arguments.session_id.0
                    );
                    Box::new(crate::acp::ACPTerminalCommandExecutor::new(
                        arguments.session_id.clone(),
                    ))
                } else {
                    if terminal_supported {
                        tracing::warn!("ACP: No client connection available, falling back to DefaultCommandExecutor");
                    } else {
                        tracing::info!("ACP: Client does not advertise terminal support; using DefaultCommandExecutor");
                    }
                    Box::new(DefaultCommandExecutor)
                }
            };

            // Mark session as connected so ProxyUI forwards to our UI
            {
                let mut manager = session_manager.lock().await;
                if let Some(session) = manager.get_session_mut(&arguments.session_id.0) {
                    session.set_ui_connected(true);
                    tracing::debug!("ACP: Marked session as UI-connected");
                } else {
                    let error_msg = "Session not found when trying to mark as connected";
                    tracing::error!("{}", error_msg);
                    send_error(error_msg.to_string()).await;
                    let mut uis = active_uis.lock().await;
                    uis.remove(arguments.session_id.0.as_ref());
                    return Ok(acp::PromptResponse {
                        stop_reason: acp::StopReason::EndTurn,
                        meta: None,
                    });
                }
            }

            // Start agent
            if let Err(e) = async {
                let launch_resources = AgentLaunchResources {
                    llm_provider: llm_client,
                    project_manager,
                    command_executor,
                    ui: ui.clone(),
                    session_llm_config: Some(session_llm_config),
                };

                let mut manager = session_manager.lock().await;
                manager
                    .start_agent_for_message(
                        &arguments.session_id.0,
                        content_blocks,
                        launch_resources,
                    )
                    .await
            }
            .await
            {
                let error_msg = format!("Failed to start agent: {e}");
                tracing::error!("{}", error_msg);
                send_error(error_msg).await;
                let mut uis = active_uis.lock().await;
                uis.remove(arguments.session_id.0.as_ref());
                return Ok(acp::PromptResponse {
                    stop_reason: acp::StopReason::EndTurn,
                    meta: None,
                });
            }

            // Wait for agent to complete
            // The agent will send session/update events via ACPUserUI as it processes
            tracing::info!("ACP: Waiting for agent to complete");
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                let is_idle = {
                    let manager = session_manager.lock().await;
                    if let Some(session) = manager.get_session(&arguments.session_id.0) {
                        let state = session.get_activity_state();
                        tracing::trace!("ACP: Session state: {:?}", state);
                        state == SessionActivityState::Idle
                    } else {
                        tracing::warn!("ACP: Session not found in manager");
                        true
                    }
                };

                if is_idle {
                    tracing::info!("ACP: Agent is idle, exiting wait loop");
                    break;
                }

                // Check if we should continue
                if !ui.should_streaming_continue() {
                    tracing::info!("ACP: Streaming cancelled");

                    // Mark session as disconnected and remove UI from active set
                    {
                        let mut manager = session_manager.lock().await;
                        if let Some(session) = manager.get_session_mut(&arguments.session_id.0) {
                            session.set_ui_connected(false);
                        }
                    }

                    {
                        let mut uis = active_uis.lock().await;
                        uis.remove(arguments.session_id.0.as_ref());
                    }

                    return Ok(acp::PromptResponse {
                        stop_reason: acp::StopReason::Cancelled,
                        meta: None,
                    });
                }
            }

            tracing::info!(
                "ACP: Prompt completed for session: {}",
                arguments.session_id.0
            );

            // Mark session as disconnected and remove UI from active set
            {
                let mut manager = session_manager.lock().await;
                if let Some(session) = manager.get_session_mut(&arguments.session_id.0) {
                    session.set_ui_connected(false);
                    tracing::debug!("ACP: Marked session as UI-disconnected");
                }
            }

            {
                let mut uis = active_uis.lock().await;
                uis.remove(arguments.session_id.0.as_ref());
            }

            Ok(acp::PromptResponse {
                stop_reason: acp::StopReason::EndTurn,
                meta: None,
            })
        })
    }

    fn cancel<'life0, 'async_trait>(
        &'life0 self,
        args: acp::CancelNotification,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), acp::Error>> + 'async_trait>>
    where
        Self: 'async_trait,
        'life0: 'async_trait,
    {
        let session_manager = self.session_manager.clone();
        let active_uis = self.active_uis.clone();

        Box::pin(async move {
            tracing::info!("ACP: Received cancel for session: {}", args.session_id.0);

            // Signal the UI to stop (this makes prompt() loop exit)
            {
                let uis = active_uis.lock().await;
                if let Some(ui) = uis.get(args.session_id.0.as_ref()) {
                    ui.signal_cancel();
                    tracing::info!(
                        "ACP: Signaled cancel to UI for session: {}",
                        args.session_id.0
                    );
                }
            }

            // Terminate the agent task
            {
                let mut manager = session_manager.lock().await;
                if let Some(session) = manager.get_session_mut(&args.session_id.0) {
                    session.terminate_agent();
                    tracing::info!("ACP: Terminated agent for session: {}", args.session_id.0);
                }
            }

            Ok(())
        })
    }
}
