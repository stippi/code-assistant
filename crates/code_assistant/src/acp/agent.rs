use agent_client_protocol as acp;
use anyhow::Result;
use std::cell::Cell;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::acp::types::convert_prompt_to_content_blocks;
use crate::acp::ACPUserUI;
use crate::config::DefaultProjectManager;
use crate::persistence::LlmSessionConfig;
use crate::session::instance::SessionActivityState;
use crate::session::{AgentConfig, SessionManager};
use crate::ui::UserInterface;
use crate::utils::DefaultCommandExecutor;
use llm::factory::{create_llm_client, LLMClientConfig};

pub struct ACPAgentImpl {
    session_manager: Arc<Mutex<SessionManager>>,
    #[allow(dead_code)]
    agent_config: AgentConfig,
    llm_config: LLMClientConfig,
    session_update_tx: mpsc::UnboundedSender<(acp::SessionNotification, oneshot::Sender<()>)>,
    #[allow(dead_code)]
    next_session_counter: Cell<u64>,
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
            next_session_counter: Cell::new(0),
        }
    }

    /// Generate a unique session ID
    #[allow(dead_code)]
    fn generate_session_id(&self) -> String {
        let counter = self.next_session_counter.get();
        self.next_session_counter.set(counter + 1);
        format!("acp-session-{counter}")
    }

    /// Replay session history by loading messages and converting to DisplayFragments
    #[allow(dead_code)]
    async fn replay_session_history(&self, session_id: &str) -> Result<()> {
        use crate::ui::streaming::create_stream_processor;

        let (tool_syntax, messages) = {
            let manager = self.session_manager.lock().await;
            let session_instance = manager
                .get_session(session_id)
                .ok_or_else(|| anyhow::anyhow!("Session not found"))?;

            (
                session_instance.session.tool_syntax,
                session_instance.session.messages.clone(),
            )
        };

        // Create a UI for this session
        let ui = Arc::new(ACPUserUI::new(
            acp::SessionId(session_id.to_string().into()),
            self.session_update_tx.clone(),
        ));

        // Create stream processor to extract fragments
        let mut processor = create_stream_processor(tool_syntax, ui.clone(), 0);

        // Process each message to extract and send fragments
        for message in messages {
            let fragments = processor
                .extract_fragments_from_message(&message)
                .map_err(|e| anyhow::anyhow!("Failed to extract fragments: {}", e))?;

            for fragment in fragments {
                ui.display_fragment(&fragment)
                    .map_err(|e| anyhow::anyhow!("Failed to display fragment: {}", e))?;
            }
        }

        Ok(())
    }
}

impl acp::Agent for ACPAgentImpl {
    fn initialize(
        &self,
        _arguments: acp::InitializeRequest,
    ) -> impl std::future::Future<Output = Result<acp::InitializeResponse, acp::Error>> {
        async move {
            tracing::info!("ACP: Received initialize request");

            Ok(acp::InitializeResponse {
                protocol_version: acp::V1,
                agent_capabilities: acp::AgentCapabilities {
                    load_session: true,
                    prompt_capabilities: acp::PromptCapabilities {
                        image: true,
                        audio: false,
                        embedded_context: true,
                    },
                },
                auth_methods: Vec::new(),
            })
        }
    }

    fn authenticate(
        &self,
        _arguments: acp::AuthenticateRequest,
    ) -> impl std::future::Future<Output = Result<(), acp::Error>> {
        async move {
            tracing::info!("ACP: Received authenticate request");
            Ok(())
        }
    }

    fn new_session(
        &self,
        _arguments: acp::NewSessionRequest,
    ) -> impl std::future::Future<Output = Result<acp::NewSessionResponse, acp::Error>> {
        let session_manager = self.session_manager.clone();
        let llm_config = self.llm_config.clone();

        async move {
            tracing::info!("ACP: Creating new session");

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
            })
        }
    }

    fn load_session(
        &self,
        arguments: acp::LoadSessionRequest,
    ) -> impl std::future::Future<Output = Result<(), acp::Error>> {
        let session_manager = self.session_manager.clone();
        let session_update_tx = self.session_update_tx.clone();

        async move {
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
            let (tool_syntax, messages) = {
                let manager = session_manager.lock().await;
                let session_instance = manager
                    .get_session(&arguments.session_id.0)
                    .ok_or_else(acp::Error::internal_error)?;

                (
                    session_instance.session.tool_syntax,
                    session_instance.session.messages.clone(),
                )
            };

            // Create a UI for this session
            let ui = Arc::new(ACPUserUI::new(
                arguments.session_id.clone(),
                session_update_tx,
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

            Ok(())
        }
    }

    fn prompt(
        &self,
        arguments: acp::PromptRequest,
    ) -> impl std::future::Future<Output = Result<acp::PromptResponse, acp::Error>> {
        let session_manager = self.session_manager.clone();
        let session_update_tx = self.session_update_tx.clone();
        let llm_config = self.llm_config.clone();

        async move {
            tracing::info!(
                "ACP: Received prompt for session: {}",
                arguments.session_id.0
            );

            // Create UI for this session
            let ui: Arc<dyn crate::ui::UserInterface> = Arc::new(ACPUserUI::new(
                arguments.session_id.clone(),
                session_update_tx,
            ));

            // Convert prompt content blocks
            let content_blocks = convert_prompt_to_content_blocks(arguments.prompt);

            // Create LLM client
            let llm_client = create_llm_client(llm_config).await.map_err(|e| {
                tracing::error!("Failed to create LLM client: {}", e);
                acp::Error::internal_error()
            })?;

            // Create project manager and command executor
            let project_manager = Box::new(DefaultProjectManager::new());
            let command_executor = Box::new(DefaultCommandExecutor);

            // Start agent
            {
                let mut manager = session_manager.lock().await;
                manager
                    .start_agent_for_message(
                        &arguments.session_id.0,
                        content_blocks,
                        llm_client,
                        project_manager,
                        command_executor,
                        ui.clone(),
                    )
                    .await
                    .map_err(|e| {
                        tracing::error!("Failed to start agent: {}", e);
                        acp::Error::internal_error()
                    })?;
            }

            // Wait for agent to complete
            // The agent will send session/update events via ACPUserUI as it processes
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                let is_idle = {
                    let manager = session_manager.lock().await;
                    if let Some(session) = manager.get_session(&arguments.session_id.0) {
                        session.get_activity_state() == SessionActivityState::Idle
                    } else {
                        true
                    }
                };

                if is_idle {
                    break;
                }

                // Check if we should continue
                if !ui.should_streaming_continue() {
                    tracing::info!("ACP: Streaming cancelled");
                    return Ok(acp::PromptResponse {
                        stop_reason: acp::StopReason::Cancelled,
                    });
                }
            }

            tracing::info!(
                "ACP: Prompt completed for session: {}",
                arguments.session_id.0
            );

            Ok(acp::PromptResponse {
                stop_reason: acp::StopReason::EndTurn,
            })
        }
    }

    fn cancel(
        &self,
        args: acp::CancelNotification,
    ) -> impl std::future::Future<Output = Result<(), acp::Error>> {
        let session_manager = self.session_manager.clone();

        async move {
            tracing::info!("ACP: Received cancel for session: {}", args.session_id.0);

            // Terminate the agent
            {
                let mut manager = session_manager.lock().await;
                if let Some(session) = manager.get_session_mut(&args.session_id.0) {
                    session.terminate_agent();
                }
            }

            Ok(())
        }
    }
}
