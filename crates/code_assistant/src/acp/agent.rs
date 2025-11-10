use agent_client_protocol as acp;
#[allow(unused_imports)]
use anyhow::Context;
use anyhow::Result;
use serde_json::{json, Map as JsonMap, Value as JsonValue};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::acp::error_handling::to_acp_error;
use crate::acp::types::convert_prompt_to_content_blocks;
use crate::acp::{ACPUserUI, AcpProjectManager};
use crate::config::{DefaultProjectManager, ProjectManager};
use crate::persistence::SessionModelConfig;
use crate::session::instance::SessionActivityState;
use crate::session::{SessionConfig, SessionManager};
use crate::ui::UserInterface;
use crate::utils::DefaultCommandExecutor;
use llm::factory::create_llm_client_from_model;
use llm::provider_config::ConfigurationSystem;

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
    session_config_template: SessionConfig,
    model_name: String,
    playback_path: Option<std::path::PathBuf>,
    fast_playback: bool,
    session_update_tx: mpsc::UnboundedSender<(acp::SessionNotification, oneshot::Sender<()>)>,
    /// Active UI instances for running prompts, keyed by session ID
    /// Used to signal cancellation to the prompt() wait loop
    active_uis: Arc<Mutex<HashMap<String, Arc<ACPUserUI>>>>,
    client_capabilities: Arc<Mutex<Option<acp::ClientCapabilities>>>,
}

struct ModelStateInfo {
    state: acp::SessionModelState,
    selected_model_name: String,
    selection_changed: bool,
}

impl ACPAgentImpl {
    pub fn new(
        session_manager: Arc<Mutex<SessionManager>>,
        session_config_template: SessionConfig,
        model_name: String,
        playback_path: Option<std::path::PathBuf>,
        fast_playback: bool,
        session_update_tx: mpsc::UnboundedSender<(acp::SessionNotification, oneshot::Sender<()>)>,
    ) -> Self {
        Self {
            session_manager,
            session_config_template,
            model_name,
            playback_path,
            fast_playback,
            session_update_tx,
            active_uis: Arc::new(Mutex::new(HashMap::new())),
            client_capabilities: Arc::new(Mutex::new(None)),
        }
    }

    fn agent_info() -> acp::Implementation {
        acp::Implementation {
            name: "code-assistant".to_string(),
            title: Some("Code Assistant".to_string()),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    fn compute_model_state(
        default_model: &str,
        preferred_model: Option<&str>,
    ) -> Option<ModelStateInfo> {
        let config = match ConfigurationSystem::load() {
            Ok(config) => config,
            Err(err) => {
                tracing::error!(error = ?err, "ACP: Failed to load configuration system for model selector");
                return None;
            }
        };

        let mut entries = Vec::new();
        let mut available_names: HashSet<String> = HashSet::new();

        for (display_name, model_config) in &config.models {
            let Some(provider_config) = config.providers.get(&model_config.provider) else {
                tracing::warn!(
                    provider = %model_config.provider,
                    model = %display_name,
                    "ACP: Skipping model because provider configuration is missing"
                );
                continue;
            };

            available_names.insert(display_name.clone());

            let description = if provider_config.label.is_empty() {
                None
            } else {
                Some(provider_config.label.clone())
            };

            let model_meta = json!({
                "provider": {
                    "id": model_config.provider,
                    "label": provider_config.label,
                    "type": provider_config.provider,
                },
                "model": {
                    "id": model_config.id,
                },
                "display_name": display_name,
            });

            entries.push((
                provider_config.label.clone(),
                acp::ModelInfo {
                    model_id: acp::ModelId(display_name.clone().into()),
                    name: display_name.clone(),
                    description,
                    meta: Some(model_meta),
                },
            ));
        }

        if entries.is_empty() {
            tracing::warn!("ACP: No available models found for model selector");
            return None;
        }

        entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));

        let preferred_display = preferred_model
            .and_then(|name| available_names.contains(name).then(|| name.to_string()));

        let default_display = available_names
            .contains(default_model)
            .then(|| default_model.to_string());

        let selected_model_name = preferred_display
            .clone()
            .or_else(|| default_display.clone())
            .or_else(|| entries.first().map(|entry| entry.1.name.clone()))
            .unwrap_or_else(|| entries[0].1.name.clone());

        let selected_model_id = selected_model_name.clone();

        let mut selection_changed = false;
        if let Some(original) = preferred_model {
            match preferred_display.as_ref() {
                Some(display) => {
                    if display != original {
                        selection_changed = true;
                    }
                    if display != &selected_model_name {
                        selection_changed = true;
                    }
                }
                None => {
                    selection_changed = true;
                }
            }
        }

        let available_models: Vec<acp::ModelInfo> =
            entries.into_iter().map(|(_, info)| info).collect();

        let mut providers_meta = JsonMap::new();
        for (provider_id, provider_config) in &config.providers {
            providers_meta.insert(
                provider_id.clone(),
                json!({
                    "label": provider_config.label,
                    "type": provider_config.provider,
                }),
            );
        }

        let default_display_value = default_display
            .clone()
            .unwrap_or_else(|| selected_model_name.clone());

        let mut state_meta = JsonMap::new();
        state_meta.insert(
            "default_model_id".into(),
            json!(default_display_value.clone()),
        );
        state_meta.insert(
            "default_model_display_name".into(),
            json!(default_display_value.clone()),
        );
        state_meta.insert(
            "current_model_display_name".into(),
            json!(selected_model_name.clone()),
        );
        state_meta.insert("providers".into(), JsonValue::Object(providers_meta));

        Some(ModelStateInfo {
            state: acp::SessionModelState {
                current_model_id: acp::ModelId(selected_model_id.clone().into()),
                available_models,
                meta: Some(JsonValue::Object(state_meta)),
            },
            selected_model_name,
            selection_changed,
        })
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

            // Early configuration validation
            if let Err(e) = ConfigurationSystem::load() {
                tracing::error!(
                    "Configuration validation failed during initialization: {}",
                    e
                );
                return Err(to_acp_error(&e));
            }

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
                    meta: Some(json!({
                    "models": {
                            "supportsModelSelector": true,
                            "idFormat": "display_name",
                        },
                    })),
                },
                auth_methods: Vec::new(),
                agent_info: Some(Self::agent_info()),
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
        let model_name = self.model_name.clone();
        let session_config_template = self.session_config_template.clone();

        Box::pin(async move {
            tracing::info!("ACP: Creating new session with cwd: {:?}", arguments.cwd);

            // Update the agent config to use the provided cwd
            let mut session_config = session_config_template.clone();
            session_config.init_path = Some(arguments.cwd.clone());

            let session_id = {
                let mut manager = session_manager.lock().await;
                let session_model_config = SessionModelConfig::new(model_name.clone());
                manager
                    .create_session_with_config(
                        None,
                        Some(session_config),
                        Some(session_model_config),
                    )
                    .map_err(|e| {
                        tracing::error!("Failed to create session: {}", e);
                        to_acp_error(&e)
                    })?
            };

            tracing::info!("ACP: Created session: {}", session_id);

            let mut models_state = None;
            if let Some(model_info) =
                ACPAgentImpl::compute_model_state(&model_name, Some(model_name.as_str()))
            {
                if model_info.selection_changed {
                    let mut manager = session_manager.lock().await;
                    let fallback_model_config =
                        SessionModelConfig::new(model_info.selected_model_name.clone());
                    if let Err(err) =
                        manager.set_session_model_config(&session_id, Some(fallback_model_config))
                    {
                        tracing::error!(
                            error = ?err,
                            "ACP: Failed to persist fallback model selection for session {}",
                            session_id
                        );
                    }
                }
                models_state = Some(model_info.state);
            }

            Ok(acp::NewSessionResponse {
                session_id: acp::SessionId(session_id.into()),
                modes: None, // TODO: Support modes like "Plan", "Architect" and "Code".
                models: models_state,
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
        let default_model_name = self.model_name.clone();

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
            let (tool_syntax, messages, base_path, stored_model_config) = {
                let manager = session_manager.lock().await;
                let session_instance = manager
                    .get_session(&arguments.session_id.0)
                    .ok_or_else(acp::Error::internal_error)?;

                (
                    session_instance.session.config.tool_syntax,
                    session_instance.session.messages.clone(),
                    session_instance.session.config.init_path.clone(),
                    session_instance.session.model_config.clone(),
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

            let mut models_state = None;
            if let Some(model_info) = ACPAgentImpl::compute_model_state(
                &default_model_name,
                stored_model_config
                    .as_ref()
                    .map(|config| config.model_name.as_str()),
            ) {
                if model_info.selection_changed {
                    let mut manager = session_manager.lock().await;
                    let fallback_model_config =
                        SessionModelConfig::new(model_info.selected_model_name.clone());
                    if let Err(err) = manager.set_session_model_config(
                        &arguments.session_id.0,
                        Some(fallback_model_config),
                    ) {
                        tracing::error!(
                            error = ?err,
                            "ACP: Failed to persist fallback model selection while loading session {}",
                            arguments.session_id.0
                        );
                    }
                }
                models_state = Some(model_info.state);
            }

            tracing::info!("ACP: Loaded session: {}", arguments.session_id.0);

            Ok(acp::LoadSessionResponse {
                modes: None,
                models: models_state,
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
        let model_name = self.model_name.clone();
        let playback_path = self.playback_path.clone();
        let fast_playback = self.fast_playback;
        let active_uis = self.active_uis.clone();
        let client_capabilities = self.client_capabilities.clone();
        let client_connection = get_acp_client_connection();

        Box::pin(async move {
            tracing::info!(
                "ACP: Received prompt for session: {}",
                arguments.session_id.0
            );

            let terminal_supported = {
                let caps = client_capabilities.lock().await;
                caps.as_ref().map(|caps| caps.terminal).unwrap_or(false)
            };

            let filesystem_supported = {
                let caps = client_capabilities.lock().await;
                caps.as_ref()
                    .map(|caps| caps.fs.read_text_file && caps.fs.write_text_file)
                    .unwrap_or(false)
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

            // Convert prompt content blocks
            let content_blocks = convert_prompt_to_content_blocks(arguments.prompt);

            let config_result = {
                let manager = session_manager.lock().await;
                manager.get_session_model_config(&arguments.session_id.0)
            };

            let session_model_config = match config_result {
                Ok(Some(config)) => config,
                Ok(None) => SessionModelConfig::new(model_name.clone()),
                Err(e) => {
                    let error_msg = format!(
                        "Failed to load session model configuration for session {}: {e}",
                        arguments.session_id.0
                    );
                    tracing::error!("{}", error_msg);
                    let mut uis = active_uis.lock().await;
                    uis.remove(arguments.session_id.0.as_ref());
                    let err = e.context(error_msg);
                    return Err(to_acp_error(&err));
                }
            };

            let model_name_for_prompt = session_model_config.model_name.clone();

            // Create LLM client
            let llm_client = match create_llm_client_from_model(
                &model_name_for_prompt,
                playback_path,
                fast_playback,
                None,
            )
            .await
            {
                Ok(client) => client,
                Err(e) => {
                    let error_msg = format!(
                        "Failed to create LLM client for model '{model_name_for_prompt}': {e}"
                    );
                    tracing::error!("{}", error_msg);
                    let mut uis = active_uis.lock().await;
                    uis.remove(arguments.session_id.0.as_ref());
                    let err = e.context(error_msg);
                    return Err(to_acp_error(&err));
                }
            };

            let use_acp_fs = filesystem_supported && client_connection.is_some();

            // Create project manager and command executor
            let project_manager: Box<dyn ProjectManager> = if use_acp_fs {
                Box::new(AcpProjectManager::new(
                    DefaultProjectManager::new(),
                    arguments.session_id.clone(),
                ))
            } else {
                Box::new(DefaultProjectManager::new())
            };

            // Use ACP Terminal Command Executor if client connection is available
            let command_executor: Box<dyn crate::utils::command::CommandExecutor> = {
                if terminal_supported && client_connection.is_some() {
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
                    let mut uis = active_uis.lock().await;
                    uis.remove(arguments.session_id.0.as_ref());
                    return Err(to_acp_error(&anyhow::anyhow!(error_msg)));
                }
            }

            // Start agent
            if let Err(e) = async {
                let mut manager = session_manager.lock().await;
                manager.set_session_model_config(
                    &arguments.session_id.0,
                    Some(session_model_config.clone()),
                )?;
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
            }
            .await
            {
                let error_msg = format!("Failed to start agent: {e}");
                tracing::error!("{}", error_msg);
                {
                    let mut manager = session_manager.lock().await;
                    if let Some(session) = manager.get_session_mut(&arguments.session_id.0) {
                        session.set_ui_connected(false);
                    }
                }
                let mut uis = active_uis.lock().await;
                uis.remove(arguments.session_id.0.as_ref());
                let err = e.context(error_msg);
                return Err(to_acp_error(&err));
            }

            // Wait for agent to complete and check for errors
            tracing::info!("ACP: Waiting for agent to complete");
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                let (is_idle, task_result) = {
                    let mut manager = session_manager.lock().await;
                    if let Some(session) = manager.get_session_mut(&arguments.session_id.0) {
                        let state = session.get_activity_state();
                        tracing::trace!("ACP: Session state: {:?}", state);

                        let task_result = if state == SessionActivityState::Idle {
                            // Check if the task completed with an error
                            if let Some(task_handle) = session.task_handle.take() {
                                if task_handle.is_finished() {
                                    Some(task_handle.await.unwrap_or_else(|e| {
                                        Err(anyhow::anyhow!("Task panicked: {e}"))
                                    }))
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        };

                        (state == SessionActivityState::Idle, task_result)
                    } else {
                        tracing::warn!("ACP: Session not found in manager");
                        (true, None)
                    }
                };

                if is_idle {
                    tracing::info!("ACP: Agent is idle, exiting wait loop");

                    // Check if the agent task failed
                    if let Some(Err(e)) = task_result {
                        tracing::error!("ACP: Agent task failed: {}", e);
                        {
                            let mut manager = session_manager.lock().await;
                            if let Some(session) = manager.get_session_mut(&arguments.session_id.0)
                            {
                                session.set_ui_connected(false);
                            }
                        }
                        let mut uis = active_uis.lock().await;
                        uis.remove(arguments.session_id.0.as_ref());
                        return Err(to_acp_error(&e));
                    }

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

            if let Some(message) = acp_ui.take_last_error() {
                tracing::error!(
                    "ACP: Prompt completed with UI error for session {}: {}",
                    arguments.session_id.0,
                    message
                );
                return Err(to_acp_error(&anyhow::anyhow!(message)));
            }

            Ok(acp::PromptResponse {
                stop_reason: acp::StopReason::EndTurn,
                meta: None,
            })
        })
    }

    #[allow(clippy::manual_async_fn)]
    fn set_session_model<'life0, 'async_trait>(
        &'life0 self,
        arguments: acp::SetSessionModelRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<acp::SetSessionModelResponse, acp::Error>>
                + 'async_trait,
        >,
    >
    where
        Self: 'async_trait,
        'life0: 'async_trait,
    {
        let session_manager = self.session_manager.clone();
        let session_id = arguments.session_id.clone();
        let requested_model_id = arguments.model_id.to_string();

        Box::pin(async move {
            let config = match ConfigurationSystem::load() {
                Ok(config) => config,
                Err(err) => {
                    tracing::error!(
                        error = ?err,
                        "ACP: Failed to load configuration while setting session model"
                    );
                    return Err(to_acp_error(&err));
                }
            };

            if config.get_model(&requested_model_id).is_none() {
                tracing::warn!(
                    model_id = %requested_model_id,
                    "ACP: Received invalid model selection request"
                );
                return Err(acp::Error::invalid_params());
            }

            let display_name = requested_model_id.clone();

            let existing_config = {
                let manager = session_manager.lock().await;
                manager.get_session_model_config(&session_id.0)
            };

            if let Err(err) = existing_config {
                tracing::error!(
                    error = ?err,
                    "ACP: Failed to read existing session model configuration"
                );
                return Err(acp::Error::internal_error());
            }

            {
                let mut manager = session_manager.lock().await;
                let new_model_config = SessionModelConfig::new(display_name.clone());
                if let Err(err) =
                    manager.set_session_model_config(&session_id.0, Some(new_model_config))
                {
                    tracing::error!(
                        error = ?err,
                        "ACP: Failed to persist session model selection"
                    );
                    return Err(acp::Error::internal_error());
                }
            }

            tracing::info!(
                "ACP: Session {} switched to model {}",
                session_id.0,
                display_name,
            );

            Ok(acp::SetSessionModelResponse {
                meta: Some(json!({
                    "model": {
                        "id": display_name,
                        "display_name": display_name,
                    }
                })),
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
