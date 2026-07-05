use anyhow::{Context as _, Result};
use llm::{ContentBlock, Message};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::Mutex;

use crate::agent::{Agent, AgentComponents, DefaultSubAgentRunner, SubAgentCancellationRegistry};
use crate::config::ProjectManager;
use crate::persistence::{
    generate_session_id, ChatMetadata, ChatSession, FileSessionPersistence, SessionModelConfig,
};
use crate::session::instance::SessionInstance;
use crate::session::sleep_inhibitor::SleepInhibitor;
use crate::session::{SessionConfig, SessionState};
use crate::ui::ui_events::UiEvent;
use crate::utils::file_utils;
use command_executor::{CommandExecutor, SandboxedCommandExecutor};
use llm::LLMProvider;
use sandbox::SandboxPolicy;
use tools_core::permissions::PermissionMediator;
use tracing::{debug, error, info, warn};

/// Result of checking whether a session may switch to another model.
#[derive(Debug, Clone)]
pub struct ModelSwitchCheck {
    pub model_name: String,
    pub allowed: bool,
    pub reason: Option<String>,
}

impl ModelSwitchCheck {
    pub fn allowed(model_name: impl Into<String>) -> Self {
        Self {
            model_name: model_name.into(),
            allowed: true,
            reason: None,
        }
    }

    pub fn blocked(model_name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            model_name: model_name.into(),
            allowed: false,
            reason: Some(reason.into()),
        }
    }

    pub fn error_message(&self) -> String {
        self.reason.clone().unwrap_or_else(|| {
            format!(
                "Model '{}' cannot be selected for the current session.",
                self.model_name
            )
        })
    }
}

/// Result of a successful model switch.
#[derive(Debug, Clone)]
pub struct ModelSwitchOutcome {
    pub warning: Option<String>,
}

/// The main SessionManager that manages multiple active sessions with on-demand agents
pub struct SessionManager {
    /// Persistence layer for saving/loading sessions
    persistence: FileSessionPersistence,

    /// Active session instances (session_id -> SessionInstance)
    /// These can have running agents
    active_sessions: HashMap<String, SessionInstance>,

    /// The currently UI-active session ID
    active_session_id: Option<String>,

    /// Template configuration applied to each new session
    session_config_template: SessionConfig,

    /// Default model name to use when creating sessions
    default_model_name: String,

    /// CLI-level override that forces every new session into the diff-format
    /// editing tool, regardless of the model's `edit_format` preference.
    ///
    /// This corresponds to `--use-diff-format` on the command line. When
    /// false, the model's `edit_format` from `models.json` is honored.
    force_diff_format: bool,

    /// Prevents system idle sleep while any agent is running.
    /// Shared with spawned agent tasks so they can signal completion.
    sleep_inhibitor: Arc<SleepInhibitor>,

    /// The tool registry shared by all sessions this manager runs.
    tool_registry: Arc<crate::tools::core::ToolRegistry>,

    /// Builds the hook set for each agent this manager starts; `None` uses
    /// code-assistant's default hooks.
    hooks_factory: Option<agent_core::hooks::HookRegistryFactory>,

    /// The core→UI broadcast stream all sessions publish to.
    events: crate::session::event_stream::EventStream,
}

impl SessionManager {
    /// Create a new SessionManager.
    ///
    /// On creation, this will clean up any empty sessions from previous runs.
    /// This handles the case where a client (e.g., Zed) starts code-assistant
    /// and creates a session, but the user never sends a message before closing.
    pub fn new(
        mut persistence: FileSessionPersistence,
        session_config_template: SessionConfig,
        default_model_name: String,
        tool_registry: Arc<crate::tools::core::ToolRegistry>,
        events: crate::session::event_stream::EventStream,
    ) -> Self {
        // Clean up empty sessions from previous runs at startup
        match persistence.delete_empty_sessions() {
            Ok(count) if count > 0 => {
                info!("Cleaned up {} empty session(s) from previous runs", count);
            }
            Ok(_) => {}
            Err(e) => {
                warn!("Failed to clean up empty sessions at startup: {}", e);
            }
        }

        // The CLI's `--use-diff-format` flag is plumbed through the template's
        // `use_diff_blocks` field. Capture it as the override and reset the
        // template so subsequent per-session resolution from `models.json`
        // isn't shadowed by a stale template value.
        let force_diff_format = session_config_template.use_diff_blocks;
        let mut session_config_template = session_config_template;
        session_config_template.use_diff_blocks = false;

        Self {
            persistence,
            active_sessions: HashMap::new(),
            active_session_id: None,
            session_config_template,
            default_model_name,
            force_diff_format,
            sleep_inhibitor: Arc::new(SleepInhibitor::default()),
            tool_registry,
            hooks_factory: None,
            events,
        }
    }

    /// Install a hook factory applied to every agent this manager starts.
    /// Embedders use this to customize e.g. the system prompt provider.
    pub fn set_hooks_factory(&mut self, factory: agent_core::hooks::HookRegistryFactory) {
        self.hooks_factory = Some(factory);
    }

    /// Replace the tool registry shared by the sessions this manager runs.
    /// Wiring layers use this when the full registry is only available after
    /// asynchronous setup (e.g. connecting MCP servers) — swap before the
    /// first agent starts; already-created session instances keep the
    /// registry they were built with.
    pub fn set_tool_registry(&mut self, tool_registry: Arc<crate::tools::core::ToolRegistry>) {
        self.tool_registry = tool_registry;
    }

    /// The core→UI broadcast stream this manager's sessions publish to.
    pub fn event_stream(&self) -> &crate::session::event_stream::EventStream {
        &self.events
    }

    /// Returns the session config template.
    pub fn session_config_template(&self) -> &SessionConfig {
        &self.session_config_template
    }

    /// Create a new session and return its ID
    pub fn create_session(&mut self, name: Option<String>) -> Result<String> {
        // Always create sessions with a default model config
        let default_model_config = self.default_model_config();
        self.create_session_with_config(name, None, Some(default_model_config))
    }

    /// Get the default model configuration
    fn default_model_config(&self) -> SessionModelConfig {
        SessionModelConfig::new(self.default_model_name.clone())
    }

    /// Update the default model name used for newly created sessions.
    pub fn set_default_model_name(&mut self, model_name: String) {
        self.default_model_name = model_name;
    }

    /// Resolve whether a new session should use the diff-format edit tool.
    ///
    /// Precedence:
    /// 1. CLI override (`force_diff_format`, set via `--use-diff-format`).
    /// 2. The model's `edit_format` from `models.json`.
    /// 3. Otherwise `false` (the simple `edit` tool).
    fn resolve_use_diff_blocks(&self, model_config: Option<&SessionModelConfig>) -> bool {
        if self.force_diff_format {
            return true;
        }
        let model_name = model_config
            .map(|c| c.model_name.as_str())
            .unwrap_or(self.default_model_name.as_str());
        llm::provider_config::edit_format_for_model(model_name).is_diff()
    }

    /// Create a new session with optional model config and return its ID
    pub fn create_session_with_config(
        &mut self,
        name: Option<String>,
        session_config_override: Option<SessionConfig>,
        model_config: Option<SessionModelConfig>,
    ) -> Result<String> {
        let session_id = generate_session_id();
        self.create_session_with_id(session_id, name, session_config_override, model_config)
    }

    /// Create a new session with a specific ID (used for deferred session creation in ACP)
    pub fn create_session_with_id(
        &mut self,
        session_id: String,
        name: Option<String>,
        session_config_override: Option<SessionConfig>,
        model_config: Option<SessionModelConfig>,
    ) -> Result<String> {
        let session_name = name.unwrap_or_default(); // Empty string if no name provided

        let mut config =
            session_config_override.unwrap_or_else(|| self.session_config_template.clone());

        // Resolve `use_diff_blocks` from the chosen model's `edit_format`,
        // unless the CLI override (`--use-diff-format`) forces it on.
        // The value is stamped into SessionConfig and locked for the
        // lifetime of the session — it must not change later, since
        // mid-session tool-layout swaps would corrupt the tool-call
        // history.
        config.use_diff_blocks = self.resolve_use_diff_blocks(model_config.as_ref());

        let session =
            ChatSession::new_empty(session_id.clone(), session_name, config, model_config);

        // Save to persistence
        self.persistence.save_chat_session(&session)?;

        // Create session instance
        let instance = SessionInstance::new(session, self.tool_registry.clone());

        // Add to active sessions
        self.active_sessions.insert(session_id.clone(), instance);

        Ok(session_id)
    }

    /// Load a session from persistence and make it active
    pub fn load_session(&mut self, session_id: &str) -> Result<Vec<Message>> {
        // Load from persistence
        let session = self
            .persistence
            .load_chat_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;

        let messages = session.messages.clone();

        // Create session instance
        let instance = SessionInstance::new(session, self.tool_registry.clone());

        // Add to active sessions
        self.active_sessions
            .insert(session_id.to_string(), instance);

        Ok(messages)
    }

    /// Set the UI-active session and return an owned snapshot for rendering.
    ///
    /// When `edit_until_node_id` is `Some(_)`, the snapshot transcript is
    /// truncated to messages up to and including that node. This lets the
    /// UI restore an in-progress message edit (banner + truncated transcript)
    /// directly when connecting to a session whose draft is in edit mode,
    /// rather than loading the full transcript and then truncating it.
    pub async fn set_active_session(
        &mut self,
        session_id: String,
        edit_until_node_id: Option<crate::persistence::NodeId>,
    ) -> Result<crate::session::SessionSnapshot> {
        // Check if session exists
        let session_exists = self.active_sessions.contains_key(&session_id);

        // Load session if it doesn't exist
        if !session_exists {
            self.load_session(&session_id)?;
        }

        // Ensure the session has a model configuration and capture it for UI update
        let mut needs_persist = false;
        {
            let session_instance = self.active_sessions.get_mut(&session_id).unwrap();

            // Reload session from persistence to get latest state
            // This ensures we see any changes made by agents since session was loaded
            session_instance.reload_from_persistence(&self.persistence)?;
        }

        let model_name_for_event = {
            let existing_model_name = {
                let session_instance = self.active_sessions.get_mut(&session_id).unwrap();
                session_instance
                    .session
                    .model_config
                    .as_ref()
                    .map(|config| config.model_name.clone())
            };

            if let Some(model_name) = existing_model_name {
                model_name
            } else {
                let default_model_config = self.default_model_config();
                let model_name = default_model_config.model_name.clone();
                {
                    let session_instance = self.active_sessions.get_mut(&session_id).unwrap();
                    session_instance.session.model_config = Some(default_model_config.clone());
                }
                needs_persist = true;
                model_name
            }
        };

        // Build the owned snapshot for the frontend
        let mut snapshot = {
            let session_instance = self.active_sessions.get_mut(&session_id).unwrap();
            let snapshot = session_instance.build_snapshot(edit_until_node_id)?;
            // Mark what the UI now knows about (baseline for future incremental diffs)
            session_instance.last_ui_synced_path = session_instance.session.active_path.clone();
            session_instance.last_ui_synced_tool_count =
                session_instance.session.tool_executions.len();
            snapshot
        };

        snapshot.current_model = model_name_for_event;
        snapshot.allowed_models = self
            .allowed_models_for_session(&session_id)
            .unwrap_or_default();

        // Check if another process holds the agent lock for this session.
        // If so, mark it as RunningExternally so the UI disables input.
        if self.is_agent_locked_externally(&session_id) {
            debug!(
                "Session {} has an agent running in another process",
                session_id
            );
            snapshot.activity_state =
                crate::session::instance::SessionActivityState::RunningExternally;
        }

        // Set as active
        self.active_session_id = Some(session_id.clone());

        // Persist session if we had to backfill a default model configuration
        if needs_persist {
            let session_snapshot = {
                let session_instance = self.active_sessions.get(&session_id).unwrap();
                session_instance.session.clone()
            };
            self.persistence.save_chat_session(&session_snapshot)?;
        }

        Ok(snapshot)
    }

    /// Incremental refresh of the currently viewed session.
    ///
    /// Reloads the session from persistence and compares the on-disk
    /// `active_path` against `last_ui_synced_path` (what the UI already shows).
    ///
    /// Returns:
    /// - `Ok(vec![])` if nothing changed or the UI already has these messages
    /// - `Ok(vec![AppendMessages { .. }])` if new nodes were appended externally
    /// - `Ok(vec![SetMessages { .. }])` if the paths diverged (full reload fallback)
    ///
    /// The key insight: we compare against `last_ui_synced_path` rather than
    /// the pre-reload `session.active_path`. This eliminates the race between
    /// a locally running agent's final `save_state()` and the file-watcher
    /// debounce — even if the state has already transitioned to Idle by the
    /// time the debounce fires, `last_ui_synced_path` was advanced during
    /// prior "while running" reloads, so no spurious diff is detected.
    pub fn refresh_session_incremental(&mut self, session_id: &str) -> Result<Vec<UiEvent>> {
        let session_instance = self
            .active_sessions
            .get_mut(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;

        // If the agent is running locally, streaming keeps the UI up-to-date.
        // Just reload from persistence to advance the baseline so future diffs
        // (after the agent finishes) don't see already-streamed content as "new".
        let activity = session_instance.get_activity_state();
        if !activity.is_terminal() && !activity.is_running_externally() {
            session_instance.reload_from_persistence(&self.persistence)?;
            session_instance.last_ui_synced_path = session_instance.session.active_path.clone();
            session_instance.last_ui_synced_tool_count =
                session_instance.session.tool_executions.len();
            return Ok(Vec::new());
        }

        // Reload from disk
        session_instance.reload_from_persistence(&self.persistence)?;

        let new_path = session_instance.session.active_path.clone();
        let new_tool_count = session_instance.session.tool_executions.len();

        // Compare against what the UI already has (not the pre-reload disk state).
        let synced_path = &session_instance.last_ui_synced_path;
        let synced_tool_count = session_instance.last_ui_synced_tool_count;

        // Case 1: Paths identical and no new tool results → no-op
        if new_path == *synced_path && new_tool_count == synced_tool_count {
            return Ok(Vec::new());
        }

        // Case 1b: Paths identical but new tool results appeared
        if new_path == *synced_path {
            let all_tool_results = session_instance.convert_tool_executions_to_ui_data()?;
            let new_tool_results: Vec<_> = all_tool_results
                .into_iter()
                .skip(synced_tool_count)
                .collect();
            if new_tool_results.is_empty() {
                return Ok(Vec::new());
            }
            session_instance.last_ui_synced_tool_count = new_tool_count;
            return Ok(vec![UiEvent::AppendMessages {
                messages: Vec::new(),
                tool_results: new_tool_results,
            }]);
        }

        // Case 2: Synced path is a strict prefix of new path → append only new nodes
        if new_path.len() > synced_path.len() && new_path.starts_with(synced_path) {
            debug!(
                "Incremental refresh for {session_id}: {} new node(s)",
                new_path.len() - synced_path.len()
            );

            let tool_syntax = session_instance.session.config.tool_syntax;
            let new_node_ids = &new_path[synced_path.len()..];

            let messages_data =
                session_instance.convert_messages_from_nodes(new_node_ids, tool_syntax)?;

            let all_tool_results = session_instance.convert_tool_executions_to_ui_data()?;
            let new_tool_results: Vec<_> = all_tool_results
                .into_iter()
                .skip(synced_tool_count)
                .collect();

            let mut events = Vec::new();

            if !messages_data.is_empty() || !new_tool_results.is_empty() {
                events.push(UiEvent::AppendMessages {
                    messages: messages_data,
                    tool_results: new_tool_results,
                });
            }

            events.push(UiEvent::UpdatePlan {
                plan: session_instance.session.plan.clone(),
            });

            session_instance.last_ui_synced_path = new_path;
            session_instance.last_ui_synced_tool_count = new_tool_count;

            return Ok(events);
        }

        // Case 3: Paths diverged → full reload of the transcript
        debug!("Incremental refresh for {session_id}: paths diverged, full reload");
        let snapshot = session_instance.build_snapshot(None)?;
        session_instance.last_ui_synced_path = session_instance.session.active_path.clone();
        session_instance.last_ui_synced_tool_count = session_instance.session.tool_executions.len();
        Ok(vec![
            UiEvent::SetMessages {
                messages: snapshot.messages,
                session_id: Some(snapshot.session_id),
                tool_results: snapshot.tool_results,
            },
            UiEvent::UpdatePlan {
                plan: snapshot.plan,
            },
            UiEvent::UpdateSessionMetadata {
                metadata: snapshot.metadata,
            },
        ])
    }

    /// Advance the UI-sync baseline to match the current on-disk state.
    ///
    /// Call this after the local agent finishes and its UI has already
    /// streamed all content to the client.  This ensures the subsequent
    /// file-watcher debounce will find no diff and won't replay content
    /// that was already sent via streaming.
    pub fn advance_ui_sync_baseline(&mut self, session_id: &str) -> Result<()> {
        let session_instance = self
            .active_sessions
            .get_mut(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;

        session_instance.reload_from_persistence(&self.persistence)?;
        session_instance.last_ui_synced_path = session_instance.session.active_path.clone();
        session_instance.last_ui_synced_tool_count = session_instance.session.tool_executions.len();
        Ok(())
    }

    /// Add a user message to a session and return the new node_id.
    /// This is used to add the message before displaying it in the UI,
    /// ensuring the node_id is available for the edit button.
    pub fn add_user_message(
        &mut self,
        session_id: &str,
        content_blocks: Vec<llm::ContentBlock>,
        branch_parent_id: Option<crate::persistence::NodeId>,
    ) -> Result<crate::persistence::NodeId> {
        let session_instance = self
            .active_sessions
            .get_mut(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;

        // Make sure the session instance is not stale
        session_instance.reload_from_persistence(&self.persistence)?;

        // Add structured user message to session, optionally creating a branch
        let message = Message::new_user_content(content_blocks);
        let node_id =
            session_instance.add_message_with_branch(message.clone(), branch_parent_id)?;

        // Save the session state with the new message
        self.persistence
            .save_chat_session(&session_instance.session)?;

        // Notify message observers. Messages added here enter the agent later
        // via `restore_conversation`, which deliberately does not re-notify —
        // this call is what keeps observers (transcript mirrors, external
        // memory sync) complete for user messages sent to an idle session.
        if let Some(factory) = &self.hooks_factory {
            for observer in &factory().observers {
                observer.on_message(Some(session_id), &message);
            }
        }

        // Advance the UI-synced baseline: the user message is displayed immediately
        // by the UI, so the file watcher should not treat it as "new".
        session_instance.last_ui_synced_path = session_instance.session.active_path.clone();
        session_instance.last_ui_synced_tool_count = session_instance.session.tool_executions.len();

        Ok(node_id)
    }

    /// Get branch info for all siblings of a given node.
    /// Used after creating a new branch to update the UI for all sibling nodes.
    pub fn get_sibling_branch_infos(
        &self,
        session_id: &str,
        node_id: crate::persistence::NodeId,
    ) -> Vec<(crate::persistence::NodeId, crate::persistence::BranchInfo)> {
        let Some(session_instance) = self.active_sessions.get(session_id) else {
            return Vec::new();
        };

        // Get the branch info for the new node (which contains all siblings)
        let Some(branch_info) = session_instance.session.get_branch_info(node_id) else {
            return Vec::new();
        };

        // Return branch info for each sibling (they all have the same siblings but different active_index)
        branch_info
            .sibling_ids
            .iter()
            .enumerate()
            .map(|(idx, &sibling_id)| {
                let sibling_branch_info = crate::persistence::BranchInfo {
                    parent_node_id: branch_info.parent_node_id,
                    sibling_ids: branch_info.sibling_ids.clone(),
                    active_index: idx,
                };
                (sibling_id, sibling_branch_info)
            })
            .collect()
    }

    /// Start an agent for a session (message must already be added via add_user_message)
    /// This is the key method - agents run on-demand for specific messages
    ///
    /// Convenience method that adds a user message and starts the agent in one call.
    /// This is for callers that don't need the node_id (e.g., ACP, initial task).
    #[allow(clippy::too_many_arguments)]
    pub async fn start_agent_for_message(
        &mut self,
        session_id: &str,
        content_blocks: Vec<llm::ContentBlock>,
        branch_parent_id: Option<crate::persistence::NodeId>,
        llm_provider: Box<dyn LLMProvider>,
        project_manager: Box<dyn ProjectManager>,
        command_executor: Box<dyn CommandExecutor>,
        permission_handler: Option<Arc<dyn PermissionMediator>>,
    ) -> Result<()> {
        // Add the message first
        self.add_user_message(session_id, content_blocks, branch_parent_id)?;

        // Then start the agent
        self.start_agent_for_session(
            session_id,
            llm_provider,
            project_manager,
            command_executor,
            permission_handler,
            None,
        )
        .await
    }

    /// Start an agent for a session (message must already be added via
    /// add_user_message).
    ///
    /// `tool_scope_override` restricts this run to the tools carrying the
    /// scope's capability tag (offered to the LLM and enforced at dispatch)
    /// instead of the scope derived from the session config. Per-run only:
    /// nothing is persisted, the next run derives its scope normally. Used
    /// for system-initiated turns such as a memory-only session wrap-up.
    #[allow(clippy::too_many_arguments)]
    pub async fn start_agent_for_session(
        &mut self,
        session_id: &str,
        llm_provider: Box<dyn LLMProvider>,
        project_manager: Box<dyn ProjectManager>,
        command_executor: Box<dyn CommandExecutor>,
        permission_handler: Option<Arc<dyn PermissionMediator>>,
        tool_scope_override: Option<crate::tools::core::ToolScope>,
    ) -> Result<()> {
        // Acquire exclusive cross-process agent lock.
        // This prevents another code-assistant instance from running an agent
        // for the same session concurrently.
        let sessions_dir = self.persistence.sessions_dir()?;
        let agent_lock = file_utils::try_acquire_agent_lock(&sessions_dir, session_id)?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Cannot start agent for session {session_id}: \
                     another code-assistant instance is already running an agent for this session"
                )
            })?;

        // Prepare session - need to scope the mutable borrow carefully
        let (
            session_config,
            publisher,
            session_state,
            activity,
            pending_message_ref,
            sandbox_context,
        ) = {
            let session_instance = self
                .active_sessions
                .get_mut(session_id)
                .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;

            // Make sure the session instance is not stale
            session_instance.reload_from_persistence(&self.persistence)?;

            // Note: User message should already be added via add_user_message()

            // Clone all needed data to avoid borrowing conflicts
            let name = session_instance.session.name.clone();
            let session_config = session_instance.session.config.clone();
            // A new agent run supersedes any prior stop request and the
            // previous run's live tool statuses.
            session_instance.begin_agent_run();

            let publisher = session_instance.create_publisher(self.events.clone());
            let activity = session_instance.activity.clone();
            let pending_message_ref = session_instance.pending_message.clone();

            let session_state = crate::session::SessionState {
                session_id: session_id.to_string(),
                name,
                message_nodes: session_instance.session.message_nodes.clone(),
                active_path: session_instance.session.active_path.clone(),
                next_node_id: session_instance.session.next_node_id,
                messages: session_instance.session.get_active_messages_cloned(),
                tool_executions: session_instance
                    .session
                    .tool_executions
                    .iter()
                    .map(|se| se.deserialize(self.tool_registry.as_ref()))
                    .collect::<Result<Vec<_>>>()?,

                plan: session_instance.session.plan.clone(),
                active_skills: session_instance.session.active_skills.clone(),
                config: session_config.clone(),
                next_request_id: Some(session_instance.session.next_request_id),
                model_config: session_instance.session.model_config.clone(),
            };

            // Set activity state
            session_instance
                .set_activity_state(crate::session::instance::SessionActivityState::AgentRunning);

            (
                session_config,
                publisher,
                session_state,
                activity,
                pending_message_ref,
                session_instance.sandbox_context.clone(),
            )
        };

        // Now save the session state with the user message (outside the borrow scope)
        self.save_session_state(session_state.clone())?;

        // Broadcast the initial state change
        self.events.publish_ui(
            session_id,
            crate::ui::UiEvent::UpdateSessionActivityState {
                session_id: session_id.to_string(),
                activity_state: crate::session::instance::SessionActivityState::AgentRunning,
            },
        );

        // Create agent components
        let session_manager_ref = Arc::new(Mutex::new(SessionManager::new(
            self.persistence.clone(),
            self.session_config_template.clone(),
            self.default_model_name.clone(),
            self.tool_registry.clone(),
            self.events.clone(),
        )));

        let state_storage = Box::new(crate::agent::persistence::SessionStatePersistence::new(
            session_manager_ref,
        ));
        // Saves announce the refreshed session metadata to the UI
        let state_storage = Box::new(
            crate::agent::persistence::MetadataNotifyingPersistence::new(
                state_storage,
                publisher.clone(),
            ),
        );

        let sandbox_context_clone = sandbox_context.clone();

        let command_executor: Box<dyn CommandExecutor> =
            if session_config.sandbox_policy.requires_restrictions() {
                Box::new(SandboxedCommandExecutor::new(
                    command_executor,
                    session_config.sandbox_policy.clone(),
                    Some(sandbox_context_clone.clone()),
                    Some(session_id.to_string()),
                ))
            } else {
                command_executor
            };

        let sandboxed_project_manager = Arc::new(crate::config::SandboxAwareProjectManager::new(
            project_manager,
            sandbox_context_clone.clone(),
        ));

        // Get the cancellation registry from the session instance
        let sub_agent_cancellation_registry = self
            .active_sessions
            .get(session_id)
            .map(|s| s.sub_agent_cancellation_registry.clone())
            .unwrap_or_else(|| Arc::new(SubAgentCancellationRegistry::default()));

        // Create sub-agent runner
        let model_name_for_subagent = session_state
            .model_config
            .as_ref()
            .map(|c| c.model_name.clone())
            .unwrap_or_else(|| self.default_model_name.clone());
        let sub_agent_runner: Arc<dyn crate::agent::SubAgentRunner> =
            Arc::new(DefaultSubAgentRunner::new(
                model_name_for_subagent,
                session_config.clone(),
                sandbox_context_clone,
                sub_agent_cancellation_registry.clone(),
                publisher.clone(),
                permission_handler.clone(),
                self.tool_registry.clone(),
                self.hooks_factory.clone(),
            ));

        let components = AgentComponents {
            llm_provider,
            project_manager: sandboxed_project_manager,
            command_executor: Arc::from(command_executor),
            ui: publisher.clone(),
            state_persistence: state_storage,
            permission_handler,
            tool_registry: self.tool_registry.clone(),
            sub_agent_runner: Some(sub_agent_runner),
            hooks_factory: self.hooks_factory.clone(),
        };

        let mut agent = Agent::new(components, session_config.clone());

        // Set the shared pending message reference
        agent.set_pending_message_ref(pending_message_ref);

        // Load the session state into the agent
        agent.load_from_session_state(session_state).await?;

        // Apply the per-run scope after the load, which derives the scope
        // from the session config and would otherwise win.
        if let Some(scope) = tool_scope_override {
            agent.set_tool_scope(scope);
            agent.invalidate_system_message_cache();
        }

        // Announce the restored plan to the UI
        let _ = publisher
            .send_event(UiEvent::UpdatePlan {
                plan: agent.plan().clone(),
            })
            .await;

        // Spawn the agent task.
        //
        // The `_agent_lock` guard is moved into the task so the cross-process
        // lock is held for exactly as long as the agent is running and released
        // automatically on completion, error, panic, or task abort.
        let session_id_clone = session_id.to_string();
        let events_clone = self.events.clone();
        let sleep_inhibitor = self.sleep_inhibitor.clone();
        sleep_inhibitor.agent_started();

        let task_handle = tokio::spawn(async move {
            let _agent_lock = agent_lock; // moved in — released on drop
            debug!("Starting agent for session {}", session_id_clone);

            // Use catch_unwind to ensure cleanup runs even if the agent panics.
            // Without this, a panic leaves the session permanently stuck in
            // AgentRunning state because the cleanup code below is never reached.
            let result = {
                use futures::FutureExt;
                let iteration_future = std::panic::AssertUnwindSafe(agent.run_single_iteration());
                match iteration_future.catch_unwind().await {
                    Ok(result) => result,
                    Err(panic_payload) => {
                        let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                            (*s).to_string()
                        } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                            s.clone()
                        } else {
                            "unknown panic".to_string()
                        };
                        error!(
                            "Agent task panicked for session {}: {}",
                            session_id_clone, panic_msg
                        );
                        Err(anyhow::anyhow!("Agent panicked: {}", panic_msg))
                    }
                }
            };

            // Log the completion with detailed error information if failed
            match &result {
                Ok(()) => {
                    // Always set session state back to Idle when agent task ends successfully
                    debug!(
                        "Agent completed successfully for session {}, setting state to Idle",
                        session_id_clone
                    );
                    activity.set(crate::session::instance::SessionActivityState::Idle);

                    // Broadcast Idle to UI
                    events_clone.publish_ui(
                        &session_id_clone,
                        crate::ui::UiEvent::UpdateSessionActivityState {
                            session_id: session_id_clone.clone(),
                            activity_state: crate::session::instance::SessionActivityState::Idle,
                        },
                    );
                }
                Err(e) => {
                    error!("Agent failed for session {}: {}", session_id_clone, e);
                    // Also log the full error chain for debugging
                    debug!(
                        "Agent error chain for session {}: {:?}",
                        session_id_clone, e
                    );

                    let error_message = format!("Agent error: {e}");

                    // Set session state to Errored so the sidebar shows the error indicator
                    // and the error is displayed when the user connects to this session.
                    let errored_state = crate::session::instance::SessionActivityState::Errored {
                        message: error_message.clone(),
                    };
                    activity.set(errored_state.clone());

                    // Broadcast Errored state (sidebar update). We do NOT
                    // publish DisplayError here — frontends show the banner
                    // only when the errored session is the one being viewed
                    // (via the activity event or the snapshot's connect
                    // sequence).
                    events_clone.publish_ui(
                        &session_id_clone,
                        crate::ui::UiEvent::UpdateSessionActivityState {
                            session_id: session_id_clone.clone(),
                            activity_state: errored_state,
                        },
                    );
                }
            }

            // Signal that this agent is no longer running so the system sleep
            // inhibition can be released once all agents have finished.
            sleep_inhibitor.agent_stopped();

            result
        });

        // Update the task handle for this session
        if let Some(session_instance) = self.active_sessions.get_mut(session_id) {
            session_instance.task_handle = Some(task_handle);
        }

        Ok(())
    }

    /// List all available sessions (both active and persisted)
    pub fn list_all_sessions(&self) -> Result<Vec<ChatMetadata>> {
        self.persistence.list_chat_sessions()
    }

    /// Delete a session
    pub fn delete_session(&mut self, session_id: &str) -> Result<()> {
        // Remove from active sessions
        if let Some(mut session_instance) = self.active_sessions.remove(session_id) {
            let agent_is_running = !session_instance.get_activity_state().is_terminal();
            session_instance.terminate_agent();
            // When aborting a task, the cleanup code inside the task (including
            // agent_stopped) won't run, so we signal completion here instead.
            // We check the activity state rather than task_handle.is_some()
            // because the handle persists even after the task has completed.
            if agent_is_running {
                self.sleep_inhibitor.agent_stopped();
            }
        }

        // Clear active session if it was the deleted one
        if self.active_session_id.as_deref() == Some(session_id) {
            self.active_session_id = None;
        }

        // Delete from persistence
        self.persistence.delete_chat_session(session_id)?;

        Ok(())
    }

    /// Cancel a running sub-agent by its tool ID
    /// Returns Ok(true) if the sub-agent was found and cancelled,
    /// Ok(false) if the sub-agent was not found (may have already completed)
    pub fn cancel_sub_agent(&self, session_id: &str, tool_id: &str) -> Result<bool> {
        if let Some(session_instance) = self.active_sessions.get(session_id) {
            Ok(session_instance.cancel_sub_agent(tool_id))
        } else {
            Err(anyhow::anyhow!("Session not found: {}", session_id))
        }
    }

    /// Terminate a running agent for a session (e.g. on user cancel).
    ///
    /// This is the proper way to abort an agent task from outside `SessionManager`,
    /// because it also updates the sleep-inhibition reference count. Calling
    /// `session.terminate_agent()` directly would leak a count.
    pub fn terminate_session_agent(&mut self, session_id: &str) {
        if let Some(session) = self.active_sessions.get_mut(session_id) {
            // Check the activity state rather than task_handle.is_some() because
            // the handle persists even after the task has completed naturally.
            let agent_is_running = !session.get_activity_state().is_terminal();
            session.terminate_agent();
            if agent_is_running {
                self.sleep_inhibitor.agent_stopped();
            }
        }
    }

    /// Get a session instance by ID
    pub fn get_session(&self, session_id: &str) -> Option<&SessionInstance> {
        self.active_sessions.get(session_id)
    }

    /// Get a mutable session instance by ID
    pub fn get_session_mut(&mut self, session_id: &str) -> Option<&mut SessionInstance> {
        self.active_sessions.get_mut(session_id)
    }

    /// Get the model config for a session, if any
    pub fn get_session_model_config(&self, session_id: &str) -> Result<Option<SessionModelConfig>> {
        if let Some(instance) = self.active_sessions.get(session_id) {
            Ok(instance.session.model_config.clone())
        } else {
            // Load from persistence
            match self.persistence.load_chat_session(session_id)? {
                Some(mut session) => {
                    if let Some(config) = session.model_config.take() {
                        Ok(Some(config))
                    } else {
                        Ok(None)
                    }
                }
                None => Ok(None),
            }
        }
    }

    /// Check whether a session may switch to `model_name`.
    ///
    /// Empty sessions can switch freely. Once a session has messages or tool
    /// executions, switching is allowed only within the same provider API client
    /// family (for example Anthropic direct <-> AI Core Anthropic, or Opus 4.6
    /// <-> Opus 4.7). Cross-family switches are blocked because reasoning and
    /// tool-call history may not be decodable by the new provider.
    pub fn check_model_switch_allowed(
        &self,
        session_id: &str,
        model_name: &str,
    ) -> Result<ModelSwitchCheck> {
        let session = if let Some(instance) = self.active_sessions.get(session_id) {
            instance.session.clone()
        } else {
            self.persistence
                .load_chat_session(session_id)?
                .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?
        };

        let new_model_config = SessionModelConfig::new(model_name.to_string());
        Self::check_model_switch_for_session(&session, &new_model_config)
    }

    fn check_model_switch_for_session(
        session: &ChatSession,
        new_model_config: &SessionModelConfig,
    ) -> Result<ModelSwitchCheck> {
        let model_name = new_model_config.model_name.as_str();

        if session
            .model_config
            .as_ref()
            .is_some_and(|current| current.model_name == model_name)
        {
            return Ok(ModelSwitchCheck::allowed(model_name));
        }

        if session.message_count() == 0 && session.tool_executions.is_empty() {
            return Ok(ModelSwitchCheck::allowed(model_name));
        }

        if session.model_config.is_none() {
            return Ok(ModelSwitchCheck::allowed(model_name));
        }

        let config = llm::provider_config::ConfigurationSystem::load()?;
        Self::check_model_switch_for_session_with_config(session, new_model_config, &config)
    }

    fn check_model_switch_for_session_with_config(
        session: &ChatSession,
        new_model_config: &SessionModelConfig,
        config: &llm::provider_config::ConfigurationSystem,
    ) -> Result<ModelSwitchCheck> {
        let model_name = new_model_config.model_name.as_str();
        let Some(current_model_config) = session.model_config.as_ref() else {
            return Ok(ModelSwitchCheck::allowed(model_name));
        };

        let current_kind = config
            .model_api_client_kind(&current_model_config.model_name)
            .with_context(|| {
                format!(
                    "Failed to resolve provider API client for current model '{}'",
                    current_model_config.model_name
                )
            })?;
        let new_kind = config.model_api_client_kind(model_name).with_context(|| {
            format!("Failed to resolve provider API client for model '{model_name}'")
        })?;

        if current_kind == new_kind {
            Ok(ModelSwitchCheck::allowed(model_name))
        } else {
            Ok(ModelSwitchCheck::blocked(
                model_name,
                format!(
                    "Cannot switch this session from model '{}' ({}) to '{}' ({}). \
                     Model switching after a conversation has started is only allowed between models that use the same provider API client. \
                     Create a new session to use this model.",
                    current_model_config.model_name, current_kind, model_name, new_kind
                ),
            ))
        }
    }

    /// List models that may be selected for this session according to
    /// [`Self::check_model_switch_allowed`].
    pub fn allowed_models_for_session(&self, session_id: &str) -> Result<Vec<String>> {
        let mut models = llm::provider_config::ConfigurationSystem::load()?.list_models();
        models.sort();

        let mut allowed = Vec::new();
        for model in models {
            match self.check_model_switch_allowed(session_id, &model) {
                Ok(check) if check.allowed => allowed.push(model),
                Ok(_) => {}
                Err(e) => {
                    debug!(
                        "Skipping model '{}' for session {} because compatibility check failed: {}",
                        model, session_id, e
                    );
                }
            }
        }
        Ok(allowed)
    }

    /// Update the persisted model config for a session.
    ///
    /// For empty sessions this also updates the locked edit-tool layout from
    /// the selected model's `edit_format`. For non-empty sessions, switching is
    /// allowed only within the same provider API client family.
    pub fn set_session_model_config(
        &mut self,
        session_id: &str,
        model_config: Option<SessionModelConfig>,
    ) -> Result<ModelSwitchOutcome> {
        let mut session = self
            .persistence
            .load_chat_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;

        if let Some(new_config) = model_config.as_ref() {
            let check = Self::check_model_switch_for_session(&session, new_config)?;
            if !check.allowed {
                anyhow::bail!(check.error_message());
            }
        } else if session.message_count() > 0 || !session.tool_executions.is_empty() {
            anyhow::bail!("Cannot clear the model for a session after a conversation has started.");
        }

        let is_empty = session.message_count() == 0 && session.tool_executions.is_empty();
        if is_empty {
            session.config.use_diff_blocks = self.resolve_use_diff_blocks(model_config.as_ref());
        }

        let agent_running = self
            .active_sessions
            .get(session_id)
            .is_some_and(|instance| !instance.get_activity_state().is_terminal());

        session.model_config = model_config.clone();
        self.persistence.save_chat_session(&session)?;

        if let Some(instance) = self.active_sessions.get_mut(session_id) {
            instance.session.model_config = model_config;
            if is_empty {
                instance.session.config.use_diff_blocks = session.config.use_diff_blocks;
            }
        }

        let warning = if agent_running {
            Some("Model switch will take effect on the next agent iteration.".to_string())
        } else {
            None
        };

        Ok(ModelSwitchOutcome { warning })
    }

    pub fn set_session_sandbox_policy(
        &mut self,
        session_id: &str,
        policy: SandboxPolicy,
    ) -> Result<()> {
        let mut session = self
            .persistence
            .load_chat_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;

        session.config.sandbox_policy = policy.clone();
        self.persistence.save_chat_session(&session)?;

        if let Some(instance) = self.active_sessions.get_mut(session_id) {
            instance.session.config.sandbox_policy = policy;
        }

        Ok(())
    }

    /// Set the worktree path and branch for a session.
    ///
    /// When a worktree is set, the agent operates in the worktree directory
    /// instead of the original project path. The sandbox context is updated
    /// to allow access to the new path.
    pub fn set_session_worktree(
        &mut self,
        session_id: &str,
        worktree_path: Option<PathBuf>,
        branch: Option<String>,
    ) -> Result<()> {
        let mut session = self
            .persistence
            .load_chat_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;

        session.config.worktree_path = worktree_path.clone();
        session.config.branch = branch.clone();
        self.persistence.save_chat_session(&session)?;

        if let Some(instance) = self.active_sessions.get_mut(session_id) {
            instance.session.config.worktree_path = worktree_path.clone();
            instance.session.config.branch = branch;

            // Register the worktree path as a sandbox root so file
            // operations and commands are allowed there.
            if let Some(path) = &worktree_path {
                let _ = instance.sandbox_context.register_root(path);
            }
        }

        Ok(())
    }

    /// Resolve a project name to its filesystem path.
    ///
    /// Checks `projects.json` first.  If the name isn't a persisted project,
    /// scans all persisted sessions for one whose `initial_project` matches
    /// and returns its `init_path`.  Returns `None` when the path cannot be
    /// determined.
    pub fn resolve_project_path(&self, project_name: &str) -> Option<PathBuf> {
        // 1. Check persisted projects
        if let Ok(projects) = crate::config::load_projects() {
            if let Some(project) = projects.get(project_name) {
                return Some(project.path.clone());
            }
        }

        // 2. Scan active sessions
        for instance in self.active_sessions.values() {
            if instance.session.initial_project() == project_name {
                if let Some(path) = instance.session.config.effective_project_path() {
                    return Some(path.clone());
                }
            }
        }

        // 3. Scan persisted sessions
        if let Ok(metadata_list) = self.persistence.list_chat_sessions() {
            for meta in &metadata_list {
                if meta.initial_project == project_name {
                    if let Ok(Some(session)) = self.persistence.load_chat_session(&meta.id) {
                        if let Some(path) = session.config.effective_project_path() {
                            return Some(path.clone());
                        }
                    }
                }
            }
        }

        None
    }

    /// Check whether a session's agent lock is held by another process.
    ///
    /// Returns `true` if an external process is running an agent for this
    /// session. Used by the UI to decide whether to disable the message input.
    pub fn is_agent_locked_externally(&self, session_id: &str) -> bool {
        // If *we* have the session in our active_sessions with a running task,
        // the lock is ours, not external.
        if let Some(instance) = self.active_sessions.get(session_id) {
            if !instance.get_activity_state().is_terminal() {
                return false; // Our own agent holds the lock
            }
        }

        let Ok(sessions_dir) = self.persistence.sessions_dir() else {
            return false;
        };
        file_utils::is_agent_locked(&sessions_dir, session_id)
    }

    /// Get the latest session ID for auto-resuming
    pub fn get_latest_session_id(&self) -> Result<Option<String>> {
        self.persistence.get_latest_session_id()
    }

    /// Get metadata for a specific session
    #[allow(dead_code)]
    pub fn get_session_metadata(&self, session_id: &str) -> Result<Option<ChatMetadata>> {
        self.persistence.get_chat_session_metadata(session_id)
    }

    /// Save the current state of an active session to persistence
    pub fn save_session(&mut self, session_id: &str) -> Result<()> {
        let session_instance = self
            .active_sessions
            .get(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

        self.persistence
            .save_chat_session(&session_instance.session)
    }

    /// Save agent state to a specific session
    pub fn save_session_state(&mut self, state: SessionState) -> Result<()> {
        let mut session = self
            .persistence
            .load_chat_session(&state.session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {}", state.session_id))?;

        // Preserve session-level settings that can be changed outside the
        // running agent. A model switch while an agent is running should take
        // effect on the next iteration; the old agent must not overwrite it
        // when it saves its captured state.
        let persisted_model_config = session.model_config.clone();
        let persisted_use_diff_blocks = session.config.use_diff_blocks;

        // Update session with current state
        session.name = state.name;

        // Update tree structure.
        session.message_nodes = state.message_nodes;
        session.active_path = state.active_path;
        session.next_node_id = state.next_node_id;

        // Clear legacy messages (tree is now authoritative)
        session.messages.clear();

        session.tool_executions = state
            .tool_executions
            .into_iter()
            .map(|te| te.serialize())
            .collect::<Result<Vec<_>>>()?;
        session.plan = state.plan;
        session.active_skills = state.active_skills;
        session.config = state.config;
        session.config.use_diff_blocks = persisted_use_diff_blocks;
        session.model_config = persisted_model_config;
        session.next_request_id = state.next_request_id.unwrap_or(0);
        session.updated_at = SystemTime::now();

        self.persistence.save_chat_session(&session)?;

        // Update active session instance if it exists
        if let Some(instance) = self.active_sessions.get_mut(&state.session_id) {
            instance.session = session;
        }

        Ok(())
    }

    /// Get the current context size for the active session
    /// Returns the input tokens + cache reads from the most recent assistant message
    #[allow(dead_code)]
    pub fn get_current_context_size(&self) -> u32 {
        if let Some(session_id) = &self.active_session_id {
            if let Some(session_instance) = self.active_sessions.get(session_id) {
                return session_instance.get_current_context_size();
            }
        }
        0
    }

    /// Calculate total usage for the active session
    #[allow(dead_code)]
    pub fn get_total_session_usage(&self) -> llm::Usage {
        if let Some(session_id) = &self.active_session_id {
            if let Some(session_instance) = self.active_sessions.get(session_id) {
                return session_instance.calculate_total_usage();
            }
        }
        llm::Usage::zero()
    }

    /// Get usage data for a specific session
    #[allow(dead_code)]
    pub fn get_session_usage(&self, session_id: &str) -> Option<(u32, llm::Usage)> {
        if let Some(session_instance) = self.active_sessions.get(session_id) {
            let context_size = session_instance.get_current_context_size();
            let total_usage = session_instance.calculate_total_usage();
            Some((context_size, total_usage))
        } else {
            None
        }
    }

    /// Queue a text-only user message for a running agent session
    #[allow(dead_code)]
    pub fn queue_user_message(&mut self, session_id: &str, message: String) -> Result<()> {
        self.queue_structured_user_message(
            session_id,
            vec![ContentBlock::Text {
                text: message,
                start_time: None,
                end_time: None,
            }],
        )
    }

    /// Check for completed agent tasks and handle their results
    /// This should be called periodically to catch agent failures
    #[allow(dead_code)]
    pub async fn check_completed_tasks(&mut self) -> Result<()> {
        let mut completed_sessions = Vec::new();

        // Check all active sessions for completed tasks
        for (session_id, session_instance) in &mut self.active_sessions {
            if let Some(ref mut task_handle) = session_instance.task_handle {
                // Check if the task is finished (non-blocking)
                if task_handle.is_finished() {
                    // Task is complete, get the result
                    match task_handle.await {
                        Ok(Ok(())) => {
                            debug!(
                                "Agent task completed successfully for session {}",
                                session_id
                            );
                        }
                        Ok(Err(e)) => {
                            error!("Agent task failed for session {}: {}", session_id, e);
                            // Log the full error chain for debugging
                            debug!("Agent error chain for session {}: {:?}", session_id, e);
                            // Note: Error display is handled in the agent task itself
                        }
                        Err(join_error) => {
                            if join_error.is_cancelled() {
                                debug!("Agent task was cancelled for session {}", session_id);
                            } else if join_error.is_panic() {
                                error!(
                                    "Agent task panicked for session {}: {:?}",
                                    session_id, join_error
                                );
                            } else {
                                error!(
                                    "Agent task join error for session {}: {}",
                                    session_id, join_error
                                );
                            }
                        }
                    }
                    completed_sessions.push(session_id.clone());
                }
            }
        }

        // Clear task handles for completed sessions
        for session_id in completed_sessions {
            if let Some(session_instance) = self.active_sessions.get_mut(&session_id) {
                session_instance.task_handle = None;
            }
        }

        Ok(())
    }

    /// Queue structured content (text + attachments) for a running agent session
    pub fn queue_structured_user_message(
        &mut self,
        session_id: &str,
        content_blocks: Vec<ContentBlock>,
    ) -> Result<()> {
        let session_instance = self
            .active_sessions
            .get(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;

        let mut pending = session_instance.pending_message.lock().unwrap();
        match pending.as_mut() {
            Some(existing) => {
                // Append new content blocks to existing pending blocks
                existing.extend(content_blocks);
            }
            None => {
                *pending = Some(content_blocks);
            }
        }

        Ok(())
    }

    /// Get and clear pending message for editing (returns text-only summary for UI)
    pub fn request_pending_message_for_edit(&mut self, session_id: &str) -> Result<Option<String>> {
        // Get the active session instance and take the pending message
        let session_instance = self
            .active_sessions
            .get(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;

        let mut pending = session_instance.pending_message.lock().unwrap();
        Ok(pending
            .take()
            .map(|blocks| crate::utils::content::text_summary_from_blocks(&blocks)))
    }

    /// Get current pending message text summary without clearing it
    pub fn get_pending_message(&self, session_id: &str) -> Result<Option<String>> {
        let session_instance = self
            .active_sessions
            .get(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;

        let pending = session_instance.pending_message.lock().unwrap();
        Ok(pending
            .as_ref()
            .map(|blocks| crate::utils::content::text_summary_from_blocks(blocks)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn temp_persistence() -> (FileSessionPersistence, TempDir) {
        let dir = TempDir::new().expect("temp dir");
        let persistence = FileSessionPersistence::new_with_root_dir(dir.path().to_path_buf());
        (persistence, dir)
    }

    fn build_manager(force_diff: bool) -> (SessionManager, TempDir) {
        let (persistence, dir) = temp_persistence();
        let template = SessionConfig {
            use_diff_blocks: force_diff,
            ..SessionConfig::default()
        };
        let manager = SessionManager::new(
            persistence,
            template,
            "test-model".to_string(),
            crate::tools::test_registry(),
            crate::session::event_stream::EventStream::new(),
        );
        (manager, dir)
    }

    fn provider_config(
        provider: &str,
        config: serde_json::Value,
    ) -> llm::provider_config::ProviderConfig {
        llm::provider_config::ProviderConfig {
            label: provider.to_string(),
            provider: provider.to_string(),
            config,
        }
    }

    fn model_config(provider: &str, id: &str) -> llm::provider_config::ModelConfig {
        llm::provider_config::ModelConfig {
            provider: provider.to_string(),
            id: id.to_string(),
            config: serde_json::json!({}),
            context_token_limit: 1000,
            edit_format: llm::provider_config::EditFormat::Simple,
        }
    }

    fn model_switch_test_config() -> llm::provider_config::ConfigurationSystem {
        llm::provider_config::ConfigurationSystem {
            providers: HashMap::from([
                (
                    "anthropic-direct".to_string(),
                    provider_config("anthropic", serde_json::json!({})),
                ),
                (
                    "openai-responses".to_string(),
                    provider_config("openai-responses", serde_json::json!({})),
                ),
                (
                    "aicore".to_string(),
                    provider_config(
                        "ai-core",
                        serde_json::json!({
                            "models": {
                                "claude-aicore-id": "anthropic-deployment",
                                "gpt-aicore-id": {
                                    "deployment_id": "openai-deployment",
                                    "api_type": "openai-responses"
                                }
                            }
                        }),
                    ),
                ),
            ]),
            models: HashMap::from([
                (
                    "claude-direct".to_string(),
                    model_config("anthropic-direct", "claude-direct-id"),
                ),
                (
                    "claude-aicore".to_string(),
                    model_config("aicore", "claude-aicore-id"),
                ),
                (
                    "gpt-responses".to_string(),
                    model_config("openai-responses", "gpt-responses-id"),
                ),
                (
                    "gpt-aicore".to_string(),
                    model_config("aicore", "gpt-aicore-id"),
                ),
            ]),
        }
    }

    fn non_empty_session(model_name: &str) -> ChatSession {
        let mut session = ChatSession::new_empty(
            "session-id".to_string(),
            "test".to_string(),
            SessionConfig::default(),
            Some(SessionModelConfig::new(model_name.to_string())),
        );
        session.add_message(Message::new_user("hello"));
        session
    }

    #[test]
    fn model_switch_same_api_client_allowed_after_conversation_started() {
        let config = model_switch_test_config();
        let session = non_empty_session("claude-direct");
        let target = SessionModelConfig::new("claude-aicore".to_string());

        let check =
            SessionManager::check_model_switch_for_session_with_config(&session, &target, &config)
                .expect("compatibility check");

        assert!(check.allowed);
    }

    #[test]
    fn model_switch_different_api_client_blocked_after_conversation_started() {
        let config = model_switch_test_config();
        let session = non_empty_session("claude-direct");
        let target = SessionModelConfig::new("gpt-responses".to_string());

        let check =
            SessionManager::check_model_switch_for_session_with_config(&session, &target, &config)
                .expect("compatibility check");

        assert!(!check.allowed);
        assert!(
            check.error_message().contains("same provider API client"),
            "unexpected error message: {}",
            check.error_message()
        );
    }

    #[test]
    fn model_switch_aicore_api_type_participates_in_compatibility() {
        let config = model_switch_test_config();
        let session = non_empty_session("gpt-responses");
        let target = SessionModelConfig::new("gpt-aicore".to_string());

        let check =
            SessionManager::check_model_switch_for_session_with_config(&session, &target, &config)
                .expect("compatibility check");

        assert!(check.allowed);
    }

    #[test]
    fn save_session_state_preserves_model_switch_for_next_iteration() {
        let (mut manager, _dir) = build_manager(false);
        let session_id = manager
            .create_session_with_config(
                Some("test".to_string()),
                None,
                Some(SessionModelConfig::new("old-model".to_string())),
            )
            .expect("create session");

        let mut persisted = manager
            .persistence
            .load_chat_session(&session_id)
            .expect("load session")
            .expect("session exists");
        persisted.model_config = Some(SessionModelConfig::new("new-model".to_string()));
        persisted.config.use_diff_blocks = true;
        manager
            .persistence
            .save_chat_session(&persisted)
            .expect("save switched session");

        let captured_config = SessionConfig {
            use_diff_blocks: false,
            ..Default::default()
        };
        let mut captured_state = SessionState::from_messages(
            session_id.clone(),
            "test".to_string(),
            vec![Message::new_user("hello")],
            captured_config,
        );
        captured_state.model_config = Some(SessionModelConfig::new("old-model".to_string()));

        manager
            .save_session_state(captured_state)
            .expect("save session state");

        let saved = manager
            .persistence
            .load_chat_session(&session_id)
            .expect("load saved session")
            .expect("session exists");
        assert_eq!(
            saved
                .model_config
                .as_ref()
                .map(|config| config.model_name.as_str()),
            Some("new-model")
        );
        assert!(saved.config.use_diff_blocks);
    }

    /// CLI override (`--use-diff-format`) takes precedence regardless of
    /// the model's `edit_format` preference.
    #[test]
    fn resolve_use_diff_blocks_cli_override_wins() {
        let (manager, _dir) = build_manager(true);

        // Force-diff is on; even a model name that won't be found in
        // models.json (returns default `Simple`) should resolve to true.
        let cfg = SessionModelConfig::new("__nonexistent_test_model_xyz__".to_string());
        assert!(manager.resolve_use_diff_blocks(Some(&cfg)));

        // Same with no model config at all.
        assert!(manager.resolve_use_diff_blocks(None));
    }

    /// Without the CLI override, an unknown model name resolves to the
    /// default `EditFormat::Simple`, so `use_diff_blocks` is false.
    #[test]
    fn resolve_use_diff_blocks_unknown_model_defaults_simple() {
        let (manager, _dir) = build_manager(false);

        let cfg = SessionModelConfig::new("__nonexistent_test_model_xyz__".to_string());
        assert!(!manager.resolve_use_diff_blocks(Some(&cfg)));
        assert!(!manager.resolve_use_diff_blocks(None));
    }

    /// Constructing a `SessionManager` with a template that has
    /// `use_diff_blocks = true` should capture it as `force_diff_format`
    /// and reset the template field, so the template no longer carries
    /// the stale flag through to per-session resolution.
    #[test]
    fn session_manager_captures_force_diff_flag_from_template() {
        let (manager, _dir) = build_manager(true);
        assert!(manager.force_diff_format);
        assert!(!manager.session_config_template.use_diff_blocks);
    }
}
