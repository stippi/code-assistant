use agent_client_protocol::schema as acp;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex;

use crate::error_handling::to_acp_error;
use crate::permissions::{AcpPermissionMediator, PermissionMediator};
use crate::types::convert_prompt_to_content_blocks;
use crate::ui::SessionUpdateMessage;
use crate::{ACPTerminalCommandExecutor, ACPUserUI, AcpProjectManager, ClientConn};
use code_assistant_core::config::{DefaultProjectManager, ProjectManager};
use code_assistant_core::persistence::SessionModelConfig;
use code_assistant_core::session::{SessionConfig, SessionManager};
use code_assistant_core::ui::UserInterface;
use command_executor::{CommandExecutor, DefaultCommandExecutor};
use llm::factory::create_llm_client_from_model;
use llm::provider_config::ConfigurationSystem;
use tokio::sync::mpsc;

/// Config option id used for the model selector exposed via session config
/// options. This is what the client echoes back in `session/set_config_option`.
const MODEL_CONFIG_ID: &str = "model";

/// Pending session that hasn't been persisted yet (deferred until first prompt).
#[derive(Clone)]
struct PendingSession {
    config: SessionConfig,
    model_config: SessionModelConfig,
}

/// Shared state available to all request handlers. Cloned (via `Arc`) into each
/// handler closure registered on the connection builder.
pub struct AgentState {
    session_manager: Arc<Mutex<SessionManager>>,
    session_config_template: SessionConfig,
    model_name: String,
    tool_registry: Arc<tools_core::ToolRegistry>,
    playback_path: Option<std::path::PathBuf>,
    fast_playback: bool,
    /// Channel that `ACPUserUI` instances push session notifications into; the
    /// app-level forwarding task drains it and sends each notification to the
    /// client.
    session_update_tx: mpsc::UnboundedSender<SessionUpdateMessage>,
    /// Active UI instances for running prompts, keyed by session ID. Used to
    /// signal cancellation to the prompt wait loop.
    active_uis: Arc<Mutex<HashMap<String, Arc<ACPUserUI>>>>,
    client_capabilities: Arc<Mutex<Option<acp::ClientCapabilities>>>,
    /// Sessions created in `new_session` but not yet persisted (deferred until
    /// first prompt).
    pending_sessions: Arc<Mutex<HashMap<String, PendingSession>>>,
    /// The session ID currently connected via ACP (for cross-instance
    /// awareness). Updated on `load_session`/`new_session`.
    connected_session_id: Arc<StdMutex<Option<String>>>,
}

struct ModelConfigInfo {
    options: Vec<acp::SessionConfigOption>,
    selected_model_name: String,
    selection_changed: bool,
}

impl AgentState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_manager: Arc<Mutex<SessionManager>>,
        session_config_template: SessionConfig,
        model_name: String,
        tool_registry: Arc<tools_core::ToolRegistry>,
        playback_path: Option<std::path::PathBuf>,
        fast_playback: bool,
        session_update_tx: mpsc::UnboundedSender<SessionUpdateMessage>,
        connected_session_id: Arc<StdMutex<Option<String>>>,
    ) -> Self {
        Self {
            session_manager,
            session_config_template,
            model_name,
            tool_registry,
            playback_path,
            fast_playback,
            session_update_tx,
            active_uis: Arc::new(Mutex::new(HashMap::new())),
            client_capabilities: Arc::new(Mutex::new(None)),
            pending_sessions: Arc::new(Mutex::new(HashMap::new())),
            connected_session_id,
        }
    }

    fn agent_info() -> acp::Implementation {
        acp::Implementation::new("code-assistant", env!("CARGO_PKG_VERSION"))
            .title("Code Assistant")
    }

    /// Look up the context-window token limit for a model from configuration.
    fn model_context_limit(model_name: &str) -> Option<u64> {
        let config = ConfigurationSystem::load().ok()?;
        let model = config.get_model(model_name)?;
        let limit = model.context_token_limit;
        (limit > 0).then_some(limit as u64)
    }

    /// Build the model selector config option (a `select` dropdown grouped by
    /// provider) for the current configuration, resolving the selected model.
    fn compute_model_config(
        default_model: &str,
        preferred_model: Option<&str>,
    ) -> Option<ModelConfigInfo> {
        let config = match ConfigurationSystem::load() {
            Ok(config) => config,
            Err(err) => {
                tracing::error!(error = ?err, "ACP: Failed to load configuration system for model selector");
                return None;
            }
        };

        // Group available model display names by provider label.
        let mut by_provider: BTreeMap<String, Vec<String>> = BTreeMap::new();
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
            by_provider
                .entry(provider_config.label.clone())
                .or_default()
                .push(display_name.clone());
        }

        if available_names.is_empty() {
            tracing::warn!("ACP: No available models found for model selector");
            return None;
        }

        let preferred_display = preferred_model
            .and_then(|name| available_names.contains(name).then(|| name.to_string()));
        let default_display = available_names
            .contains(default_model)
            .then(|| default_model.to_string());

        // Deterministic fallback: first model by (provider label, name).
        let first_model = by_provider
            .iter()
            .next()
            .and_then(|(_, names)| names.iter().min().cloned());

        let selected_model_name = preferred_display
            .clone()
            .or_else(|| default_display.clone())
            .or(first_model)
            .unwrap_or_else(|| default_model.to_string());

        let selection_changed = match preferred_model {
            Some(original) => preferred_display.as_deref() != Some(original),
            None => false,
        };

        // Build grouped select options.
        let mut groups: Vec<acp::SessionConfigSelectGroup> = Vec::new();
        for (label, mut names) in by_provider {
            names.sort();
            let options = names
                .into_iter()
                .map(|name| acp::SessionConfigSelectOption::new(name.clone(), name))
                .collect::<Vec<_>>();
            groups.push(acp::SessionConfigSelectGroup::new(
                label.clone(),
                label,
                options,
            ));
        }

        let option = acp::SessionConfigOption::select(
            MODEL_CONFIG_ID,
            "Model",
            selected_model_name.clone(),
            groups,
        )
        .category(acp::SessionConfigOptionCategory::Model);

        Some(ModelConfigInfo {
            options: vec![option],
            selected_model_name,
            selection_changed,
        })
    }

    // ----- request handlers ------------------------------------------------

    pub async fn handle_initialize(
        &self,
        arguments: acp::InitializeRequest,
    ) -> Result<acp::InitializeResponse, acp::Error> {
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
            let mut caps = self.client_capabilities.lock().await;
            *caps = Some(arguments.client_capabilities.clone());
        }

        Ok(acp::InitializeResponse::new(acp::ProtocolVersion::V1)
            .agent_capabilities(
                acp::AgentCapabilities::new()
                    .load_session(true)
                    .session_capabilities(
                        acp::SessionCapabilities::new().list(acp::SessionListCapabilities::new()),
                    )
                    .prompt_capabilities(
                        acp::PromptCapabilities::new()
                            .image(true)
                            .embedded_context(true),
                    ),
            )
            .agent_info(Self::agent_info()))
    }

    pub async fn handle_authenticate(
        &self,
        _arguments: acp::AuthenticateRequest,
    ) -> Result<acp::AuthenticateResponse, acp::Error> {
        tracing::info!("ACP: Received authenticate request");
        Ok(acp::AuthenticateResponse::new())
    }

    pub async fn handle_new_session(
        &self,
        arguments: acp::NewSessionRequest,
    ) -> Result<acp::NewSessionResponse, acp::Error> {
        tracing::info!("ACP: Creating new session with cwd: {:?}", arguments.cwd);

        let session_id = code_assistant_core::persistence::generate_session_id();

        let mut session_config = self.session_config_template.clone();
        session_config.init_path = Some(arguments.cwd.clone());

        let model_info =
            Self::compute_model_config(&self.model_name, Some(self.model_name.as_str()));
        let selected_model_name = model_info
            .as_ref()
            .map(|info| info.selected_model_name.clone())
            .unwrap_or_else(|| self.model_name.clone());

        let session_model_config = SessionModelConfig::new(selected_model_name);

        {
            let mut pending = self.pending_sessions.lock().await;
            pending.insert(
                session_id.clone(),
                PendingSession {
                    config: session_config,
                    model_config: session_model_config,
                },
            );
        }

        tracing::info!("ACP: Created pending session: {}", session_id);

        {
            let mut connected = self.connected_session_id.lock().unwrap();
            *connected = Some(session_id.clone());
        }

        let config_options = model_info.map(|info| info.options);
        Ok(acp::NewSessionResponse::new(session_id).config_options(config_options))
    }

    pub async fn handle_load_session(
        &self,
        arguments: acp::LoadSessionRequest,
    ) -> Result<acp::LoadSessionResponse, acp::Error> {
        tracing::info!("ACP: Loading session: {}", arguments.session_id.0);

        {
            let mut connected = self.connected_session_id.lock().unwrap();
            *connected = Some(arguments.session_id.0.to_string());
        }

        // Load session into manager
        {
            let mut manager = self.session_manager.lock().await;
            manager.load_session(&arguments.session_id.0).map_err(|e| {
                tracing::error!("Failed to load session: {}", e);
                acp::Error::internal_error()
            })?;
        }

        // Replay message history as session/update events
        let (tool_syntax, messages, base_path, stored_model_config) = {
            let manager = self.session_manager.lock().await;
            let session_instance = manager
                .get_session(&arguments.session_id.0)
                .ok_or_else(acp::Error::internal_error)?;

            (
                session_instance.session.config.tool_syntax,
                session_instance.session.get_active_messages_cloned(),
                session_instance.session.config.init_path.clone(),
                session_instance.session.model_config.clone(),
            )
        };

        let context_limit = stored_model_config
            .as_ref()
            .and_then(|cfg| Self::model_context_limit(&cfg.model_name));

        // Create a UI for this session
        let ui = Arc::new(ACPUserUI::new(
            arguments.session_id.clone(),
            self.session_update_tx.clone(),
            base_path,
            self.tool_registry.clone(),
            context_limit,
        ));

        // Create stream processor to extract fragments
        let hidden_tools = self
            .tool_registry
            .hidden_tools(code_assistant_core::tools::core::ToolScope::Agent.tag());
        let mut processor = code_assistant_core::ui::streaming::create_stream_processor(
            tool_syntax,
            ui.clone(),
            0,
            hidden_tools,
            self.tool_registry.clone(),
        );

        for message in messages {
            if message.is_compaction_summary {
                let summary = match &message.content {
                    llm::MessageContent::Text(text) => text.trim().to_string(),
                    llm::MessageContent::Structured(blocks) => blocks
                        .iter()
                        .filter_map(|block| match block {
                            llm::ContentBlock::Text { text, .. } => Some(text.as_str()),
                            llm::ContentBlock::Thinking { thinking, .. } => Some(thinking.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                        .trim()
                        .to_string(),
                };
                let fragment =
                    code_assistant_core::ui::DisplayFragment::CompactionDivider { summary };
                ui.display_fragment(&fragment)
                    .map_err(|_| acp::Error::internal_error())?;
                continue;
            }

            if message.role == llm::MessageRole::User {
                match &message.content {
                    llm::MessageContent::Text(text) if text.trim().is_empty() => continue,
                    llm::MessageContent::Structured(blocks) => {
                        let has_tool_results = blocks
                            .iter()
                            .any(|block| matches!(block, llm::ContentBlock::ToolResult { .. }));
                        if has_tool_results {
                            continue;
                        }
                    }
                    _ => {}
                }

                let text = match &message.content {
                    llm::MessageContent::Text(text) => text.clone(),
                    llm::MessageContent::Structured(blocks) => blocks
                        .iter()
                        .filter_map(|block| match block {
                            llm::ContentBlock::Text { text, .. } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(""),
                };
                if !text.is_empty() {
                    let content = acp::ContentBlock::Text(acp::TextContent::new(text));
                    let chunk = ACPUserUI::content_chunk(content);
                    ui.queue_session_update(acp::SessionUpdate::UserMessageChunk(chunk));
                }

                if let llm::MessageContent::Structured(blocks) = &message.content {
                    for block in blocks {
                        if let llm::ContentBlock::Image {
                            media_type, data, ..
                        } = block
                        {
                            let content = acp::ContentBlock::Image(acp::ImageContent::new(
                                data.clone(),
                                media_type.clone(),
                            ));
                            let chunk = ACPUserUI::content_chunk(content);
                            ui.queue_session_update(acp::SessionUpdate::UserMessageChunk(chunk));
                        }
                    }
                }
                continue;
            }

            // Assistant messages: extract fragments and emit as AgentMessageChunk
            let fragments = processor
                .extract_fragments_from_message(&message)
                .map_err(|_| acp::Error::internal_error())?;

            for fragment in fragments {
                ui.display_fragment(&fragment)
                    .map_err(|_| acp::Error::internal_error())?;
            }
        }

        let mut config_options = None;
        if let Some(model_info) = Self::compute_model_config(
            &self.model_name,
            stored_model_config
                .as_ref()
                .map(|config| config.model_name.as_str()),
        ) {
            if model_info.selection_changed {
                let mut manager = self.session_manager.lock().await;
                let fallback_model_config =
                    SessionModelConfig::new(model_info.selected_model_name.clone());
                if let Err(err) = manager
                    .set_session_model_config(&arguments.session_id.0, Some(fallback_model_config))
                {
                    tracing::error!(
                        error = ?err,
                        "ACP: Failed to persist fallback model selection while loading session {}",
                        arguments.session_id.0
                    );
                }
            }
            config_options = Some(model_info.options);
        }

        tracing::info!("ACP: Loaded session: {}", arguments.session_id.0);

        Ok(acp::LoadSessionResponse::new().config_options(config_options))
    }

    pub async fn handle_set_config_option(
        &self,
        arguments: acp::SetSessionConfigOptionRequest,
    ) -> Result<acp::SetSessionConfigOptionResponse, acp::Error> {
        if arguments.config_id.0.as_ref() != MODEL_CONFIG_ID {
            tracing::warn!(
                config_id = %arguments.config_id.0,
                "ACP: Received set_config_option for unknown config id"
            );
            return Err(acp::Error::invalid_params());
        }

        let requested_model_id = arguments.value.0.to_string();

        let config = match ConfigurationSystem::load() {
            Ok(config) => config,
            Err(err) => {
                tracing::error!(error = ?err, "ACP: Failed to load configuration while setting model");
                return Err(to_acp_error(&err));
            }
        };

        if config.get_model(&requested_model_id).is_none() {
            tracing::warn!(model_id = %requested_model_id, "ACP: Invalid model selection request");
            return Err(acp::Error::invalid_params());
        }

        let new_model_config = SessionModelConfig::new(requested_model_id.clone());
        let session_id = arguments.session_id.clone();

        // Pending (not yet persisted) session?
        {
            let mut pending = self.pending_sessions.lock().await;
            if let Some(pending_session) = pending.get_mut(session_id.0.as_ref()) {
                pending_session.model_config = new_model_config;
                tracing::info!(
                    "ACP: Pending session {} switched to model {}",
                    session_id.0,
                    requested_model_id,
                );
                return Ok(self.model_config_response(&requested_model_id));
            }
        }

        // Persisted session — update via the session manager.
        {
            let mut manager = self.session_manager.lock().await;
            if let Err(err) =
                manager.set_session_model_config(&session_id.0, Some(new_model_config))
            {
                tracing::error!(error = ?err, "ACP: Failed to persist session model selection");
                return Err(to_acp_error(&err));
            }
        }

        tracing::info!(
            "ACP: Session {} switched to model {}",
            session_id.0,
            requested_model_id,
        );

        Ok(self.model_config_response(&requested_model_id))
    }

    /// Build a `SetSessionConfigOptionResponse` carrying the updated model
    /// selector (with `selected` marked current).
    fn model_config_response(&self, selected: &str) -> acp::SetSessionConfigOptionResponse {
        let options = Self::compute_model_config(&self.model_name, Some(selected))
            .map(|info| info.options)
            .unwrap_or_default();
        acp::SetSessionConfigOptionResponse::new(options)
    }

    pub async fn handle_cancel(&self, args: acp::CancelNotification) -> Result<(), acp::Error> {
        tracing::info!("ACP: Received cancel for session: {}", args.session_id.0);

        {
            let uis = self.active_uis.lock().await;
            if let Some(ui) = uis.get(args.session_id.0.as_ref()) {
                ui.signal_cancel();
                tracing::info!(
                    "ACP: Signaled cancel to UI for session: {}",
                    args.session_id.0
                );
            }
        }

        {
            let mut manager = self.session_manager.lock().await;
            manager.terminate_session_agent(&args.session_id.0);
            tracing::info!("ACP: Terminated agent for session: {}", args.session_id.0);
        }

        Ok(())
    }

    pub async fn handle_list_sessions(
        &self,
        arguments: acp::ListSessionsRequest,
    ) -> Result<acp::ListSessionsResponse, acp::Error> {
        tracing::info!("ACP: Listing sessions with cwd filter: {:?}", arguments.cwd);

        let manager = self.session_manager.lock().await;
        let all_sessions = manager.list_all_sessions().map_err(|e| {
            tracing::error!("Failed to list sessions: {}", e);
            acp::Error::internal_error()
        })?;

        let projects = code_assistant_core::config::load_projects().unwrap_or_default();

        let filter_path_canonical = arguments
            .cwd
            .as_ref()
            .and_then(|p| std::path::Path::new(p).canonicalize().ok());

        let filtered_sessions: Vec<acp::SessionInfo> = all_sessions
            .into_iter()
            .filter_map(|metadata| {
                let project_path = if metadata.initial_project.is_empty() {
                    None
                } else {
                    projects
                        .get(&metadata.initial_project)
                        .map(|p| p.path.clone())
                };

                if let Some(ref filter_canonical) = filter_path_canonical {
                    match &project_path {
                        Some(path) => {
                            let path_canonical = path.canonicalize().ok();
                            if path_canonical.as_ref() != Some(filter_canonical) {
                                return None;
                            }
                        }
                        None => return None,
                    }
                }

                let cwd = project_path?;

                let updated_at =
                    chrono::DateTime::<chrono::Utc>::from(metadata.updated_at).to_rfc3339();

                let title = if metadata.name.is_empty() {
                    None
                } else {
                    Some(metadata.name.clone())
                };

                Some(
                    acp::SessionInfo::new(metadata.id.clone(), cwd)
                        .title(title)
                        .updated_at(updated_at),
                )
            })
            .collect();

        tracing::info!(
            "ACP: Found {} sessions matching filter",
            filtered_sessions.len()
        );

        Ok(acp::ListSessionsResponse::new(filtered_sessions))
    }

    /// Run a prompt turn to completion. Invoked from a spawned task (so the
    /// dispatch loop keeps processing `session/cancel`), with `cx` the
    /// connection used to issue filesystem/terminal/permission requests.
    pub async fn run_prompt(
        self: Arc<Self>,
        cx: ClientConn,
        arguments: acp::PromptRequest,
    ) -> Result<acp::PromptResponse, acp::Error> {
        tracing::info!(
            "ACP: Received prompt for session: {}",
            arguments.session_id.0
        );

        // Materialize pending session on first prompt
        if let Some(pending) = self
            .pending_sessions
            .lock()
            .await
            .remove(arguments.session_id.0.as_ref())
        {
            tracing::info!(
                "ACP: Persisting session {} on first prompt",
                arguments.session_id.0
            );
            let mut manager = self.session_manager.lock().await;
            manager
                .create_session_with_id(
                    arguments.session_id.0.to_string(),
                    None,
                    Some(pending.config),
                    Some(pending.model_config),
                )
                .map_err(|e| {
                    tracing::error!("Failed to create session: {}", e);
                    to_acp_error(&e)
                })?;
        }

        let terminal_supported = {
            let caps = self.client_capabilities.lock().await;
            caps.as_ref().map(|caps| caps.terminal).unwrap_or(false)
        };
        let filesystem_supported = {
            let caps = self.client_capabilities.lock().await;
            caps.as_ref()
                .map(|caps| caps.fs.read_text_file && caps.fs.write_text_file)
                .unwrap_or(false)
        };

        let base_path = {
            let manager = self.session_manager.lock().await;
            manager
                .get_session(&arguments.session_id.0)
                .and_then(|session| session.session.config.init_path.clone())
        };

        // Resolve model config first so we can compute the context-window limit
        // before building the UI.
        let session_model_config = {
            let manager = self.session_manager.lock().await;
            match manager.get_session_model_config(&arguments.session_id.0) {
                Ok(Some(config)) => config,
                Ok(None) => SessionModelConfig::new(self.model_name.clone()),
                Err(e) => {
                    let error_msg = format!(
                        "Failed to load session model configuration for session {}: {e}",
                        arguments.session_id.0
                    );
                    tracing::error!("{}", error_msg);
                    return Err(to_acp_error(&e.context(error_msg)));
                }
            }
        };
        let model_name_for_prompt = session_model_config.model_name.clone();
        let context_limit = Self::model_context_limit(&model_name_for_prompt);

        // Create UI for this session
        let acp_ui = Arc::new(ACPUserUI::new(
            arguments.session_id.clone(),
            self.session_update_tx.clone(),
            base_path.clone(),
            self.tool_registry.clone(),
            context_limit,
        ));

        {
            let mut uis = self.active_uis.lock().await;
            uis.insert(arguments.session_id.0.to_string(), acp_ui.clone());
        }

        let ui: Arc<dyn UserInterface> = acp_ui.clone();

        let content_blocks =
            convert_prompt_to_content_blocks(arguments.prompt, base_path.as_deref());

        // Create LLM client
        let llm_client = match create_llm_client_from_model(
            &model_name_for_prompt,
            self.playback_path.clone(),
            self.fast_playback,
            None,
        )
        .await
        {
            Ok(client) => client,
            Err(e) => {
                let error_msg =
                    format!("Failed to create LLM client for model '{model_name_for_prompt}': {e}");
                tracing::error!("{}", error_msg);
                self.remove_active_ui(&arguments.session_id.0).await;
                return Err(to_acp_error(&e.context(error_msg)));
            }
        };

        let use_acp_fs = filesystem_supported;

        let acp_root = {
            let manager = self.session_manager.lock().await;
            manager
                .get_session(&arguments.session_id.0)
                .and_then(|session| session.session.config.init_path.clone())
        };

        let project_manager: Box<dyn ProjectManager> = if use_acp_fs {
            Box::new(AcpProjectManager::new(
                DefaultProjectManager::new(),
                arguments.session_id.clone(),
                cx.clone(),
                acp_root,
            ))
        } else {
            Box::new(DefaultProjectManager::new())
        };

        let command_executor: Box<dyn CommandExecutor> = if terminal_supported {
            tracing::info!(
                "ACP: Using ACPTerminalCommandExecutor for session {}",
                arguments.session_id.0
            );
            Box::new(ACPTerminalCommandExecutor::new(
                arguments.session_id.clone(),
                cx.clone(),
            ))
        } else {
            tracing::info!(
                "ACP: Client does not advertise terminal support; using DefaultCommandExecutor"
            );
            Box::new(DefaultCommandExecutor)
        };

        // Mark session as connected so ProxyUI forwards to our UI
        {
            let mut manager = self.session_manager.lock().await;
            if let Some(session) = manager.get_session_mut(&arguments.session_id.0) {
                session.set_ui_connected(true);
                tracing::debug!("ACP: Marked session as UI-connected");
            } else {
                let error_msg = "Session not found when trying to mark as connected";
                tracing::error!("{}", error_msg);
                self.remove_active_ui(&arguments.session_id.0).await;
                return Err(to_acp_error(&anyhow::anyhow!(error_msg)));
            }
        }

        let permission_handler: Option<Arc<dyn PermissionMediator>> = Some(Arc::new(
            AcpPermissionMediator::new(arguments.session_id.clone(), cx.clone(), acp_ui.clone()),
        )
            as Arc<dyn PermissionMediator>);

        // Start agent
        if let Err(e) = async {
            let mut manager = self.session_manager.lock().await;
            manager.set_session_model_config(
                &arguments.session_id.0,
                Some(session_model_config.clone()),
            )?;

            manager
                .start_agent_for_message(
                    &arguments.session_id.0,
                    content_blocks,
                    None, // ACP doesn't support branching yet
                    llm_client,
                    project_manager,
                    command_executor,
                    ui.clone(),
                    permission_handler,
                )
                .await
        }
        .await
        {
            let error_msg = format!("Failed to start agent: {e}");
            tracing::error!("{}", error_msg);
            self.set_disconnected(&arguments.session_id.0).await;
            self.remove_active_ui(&arguments.session_id.0).await;
            return Err(to_acp_error(&e.context(error_msg)));
        }

        // Wait for agent to complete and check for errors.
        tracing::info!("ACP: Waiting for agent to complete");
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            let (is_idle, task_result) = {
                let mut manager = self.session_manager.lock().await;

                if let Err(e) = manager.advance_ui_sync_baseline(&arguments.session_id.0) {
                    tracing::trace!("ACP: advance_ui_sync_baseline note: {e}");
                }

                if let Some(session) = manager.get_session_mut(&arguments.session_id.0) {
                    let state = session.get_activity_state();
                    tracing::trace!("ACP: Session state: {:?}", state);

                    let task_result =
                        if state.is_terminal() {
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

                    (state.is_terminal(), task_result)
                } else {
                    tracing::warn!("ACP: Session not found in manager");
                    (true, None)
                }
            };

            if is_idle {
                tracing::info!("ACP: Agent is idle, exiting wait loop");

                if let Some(Err(e)) = task_result {
                    tracing::error!("ACP: Agent task failed: {}", e);
                    self.set_disconnected(&arguments.session_id.0).await;
                    self.remove_active_ui(&arguments.session_id.0).await;
                    return Err(to_acp_error(&e));
                }

                break;
            }

            if !ui.should_streaming_continue() {
                tracing::info!("ACP: Streaming cancelled");
                self.set_disconnected(&arguments.session_id.0).await;
                self.remove_active_ui(&arguments.session_id.0).await;
                return Ok(acp::PromptResponse::new(acp::StopReason::Cancelled));
            }
        }

        tracing::info!(
            "ACP: Prompt completed for session: {}",
            arguments.session_id.0
        );

        self.set_disconnected(&arguments.session_id.0).await;
        self.remove_active_ui(&arguments.session_id.0).await;

        if let Some(message) = acp_ui.take_last_error() {
            tracing::error!(
                "ACP: Prompt completed with UI error for session {}: {}",
                arguments.session_id.0,
                message
            );
            return Err(to_acp_error(&anyhow::anyhow!(message)));
        }

        Ok(acp::PromptResponse::new(acp::StopReason::EndTurn))
    }

    /// Shared handle to the set of sessions with a locally running prompt.
    ///
    /// The filesystem watcher uses this to avoid replaying content for a
    /// session we are actively streaming ourselves (which would otherwise
    /// duplicate the just-streamed assistant message).
    pub fn active_uis(&self) -> Arc<Mutex<HashMap<String, Arc<ACPUserUI>>>> {
        self.active_uis.clone()
    }

    async fn remove_active_ui(&self, session_id: &str) {
        let mut uis = self.active_uis.lock().await;
        uis.remove(session_id);
    }

    async fn set_disconnected(&self, session_id: &str) {
        let mut manager = self.session_manager.lock().await;
        if let Some(session) = manager.get_session_mut(session_id) {
            session.set_ui_connected(false);
        }
    }
}
