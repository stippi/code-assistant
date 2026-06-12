//! code-assistant's agent: a thin wrapper that assembles
//! [`agent_core::AgentRuntime`] with the application's plugins, dialects,
//! services, and persistence, and keeps the session-level concerns (loading
//! session state, project registration, scope switching) that are not part
//! of the generic loop.

use crate::agent::persistence::{AgentStatePersistence, SessionStateAdapter};
use crate::config::ProjectManager;
use crate::permissions::PermissionMediator;
use crate::persistence::SessionModelConfig;
use crate::plugins::AgentAppState;
use crate::session::SessionConfig;
use crate::tools::core::ToolScope;
use crate::ui::{AgentUiAdapter, UserInterface};
use agent_core::{AgentRuntime, AgentRuntimeComponents};
use anyhow::Result;
use command_executor::CommandExecutor;
use llm::{LLMProvider, Message};
use std::sync::{Arc, Mutex};
use tracing::debug;

/// Runtime components required to construct an `Agent`.
pub struct AgentComponents {
    pub llm_provider: Box<dyn LLMProvider>,
    pub project_manager: Arc<dyn ProjectManager>,
    pub command_executor: Arc<dyn CommandExecutor>,
    pub ui: Arc<dyn UserInterface>,
    pub state_persistence: Box<dyn AgentStatePersistence>,
    pub permission_handler: Option<Arc<dyn PermissionMediator>>,

    /// The tool registry the agent selects and executes tools from.
    pub tool_registry: Arc<crate::tools::core::ToolRegistry>,

    /// Optional sub-agent runner used by the `spawn_agent` tool.
    pub sub_agent_runner: Option<Arc<dyn crate::agent::SubAgentRunner>>,
}

pub struct Agent {
    runtime: AgentRuntime,
    project_manager: Arc<dyn ProjectManager>,
    /// Translates the loop's events into the application vocabulary and
    /// attaches the session id; kept here to update that id.
    ui_adapter: Arc<AgentUiAdapter>,
}

impl Agent {
    pub fn new(components: AgentComponents, session_config: SessionConfig) -> Self {
        let AgentComponents {
            llm_provider,
            project_manager,
            command_executor,
            ui,
            state_persistence,
            permission_handler,
            tool_registry,
            sub_agent_runner,
        } = components;

        let app_state = AgentAppState::new(session_config.clone());
        let tool_capability = app_state.tool_scope.tag().to_string();

        let ui_adapter = Arc::new(AgentUiAdapter::new(ui.clone()));
        let services_provider = Arc::new(crate::plugins::CodeAssistantToolServices {
            project_manager: project_manager.clone(),
            ui,
            sub_agent_runner,
        });

        let runtime = AgentRuntime::new(AgentRuntimeComponents {
            llm_provider,
            dialect: crate::tool_dialects::dialect_for(session_config.tool_syntax),
            ui: ui_adapter.clone(),
            stream_hidden_tools: tool_registry.hidden_tools(ToolScope::Agent.tag()),
            registry: tool_registry,
            tool_capability,
            command_executor,
            permission_handler,
            services_provider,
            state_persistence: Box::new(SessionStateAdapter::new(state_persistence)),
            hooks: crate::plugins::default_hooks(),
            extensions: Box::new(app_state),
        });

        Self {
            runtime,
            project_manager,
            ui_adapter,
        }
    }

    /// The application state riding on the loop.
    fn app_state(&self) -> &AgentAppState {
        AgentAppState::of_ref(self.runtime.extensions())
    }

    /// The application state riding on the loop, mutably.
    fn app_state_mut(&mut self) -> &mut AgentAppState {
        AgentAppState::of(self.runtime.extensions_mut())
    }

    /// Enable diff blocks format for file editing (uses replace_in_file tool instead of edit)
    pub fn enable_diff_blocks(&mut self) {
        self.set_tool_scope(ToolScope::AgentWithDiffBlocks);
        self.app_state_mut().session_config.use_diff_blocks = true;
        // Clear cached system message so it gets regenerated with the new scope
        self.invalidate_system_message_cache();
    }

    /// Set the shared pending message reference from SessionInstance
    pub fn set_pending_message_ref(
        &mut self,
        pending_ref: Arc<Mutex<Option<Vec<llm::ContentBlock>>>>,
    ) {
        self.runtime.set_pending_message_ref(pending_ref);
    }

    /// Update the model hint used for selecting system prompts
    pub fn set_model_hint(&mut self, model_hint: Option<String>) {
        self.runtime.set_model_hint(model_hint);
    }

    /// Set the tool scope for this agent
    pub fn set_tool_scope(&mut self, scope: ToolScope) {
        self.app_state_mut().tool_scope = scope;
        self.runtime.set_tool_capability(scope.tag().to_string());
    }

    /// Set the session model configuration
    pub fn set_session_model_config(&mut self, config: SessionModelConfig) {
        self.app_state_mut().model_config = Some(config);
    }

    /// Set the session identity (id and name) for this agent
    pub fn set_session_identity(&mut self, session_id: String, session_name: String) {
        self.ui_adapter.set_session_id(Some(session_id.clone()));
        self.runtime.set_session_id(Some(session_id));
        self.app_state_mut().session_name = session_name;
    }

    /// Get a reference to the message history
    pub fn message_history(&self) -> &[Message] {
        self.runtime.message_history()
    }

    /// Disable naming reminders (used for tests)
    #[cfg(test)]
    pub fn disable_naming_reminders(&mut self) {
        self.app_state_mut().naming_reminders_enabled = false;
    }

    /// Set session name (used for tests)
    #[cfg(test)]
    pub(crate) fn set_session_name(&mut self, name: String) {
        self.app_state_mut().session_name = name;
    }

    /// Adds a message to the history and saves the state.
    pub fn append_message(&mut self, message: Message) -> Result<()> {
        self.runtime.append_message(message)
    }

    /// Run a single iteration of the agent loop without waiting for user input
    pub async fn run_single_iteration(&mut self) -> Result<()> {
        self.runtime.run_single_iteration().await
    }

    /// Load state from session state
    pub async fn load_from_session_state(
        &mut self,
        session_state: crate::session::SessionState,
    ) -> Result<()> {
        // Restore all state components
        self.ui_adapter
            .set_session_id(Some(session_state.session_id.clone()));
        self.runtime
            .set_session_id(Some(session_state.session_id));

        debug!(
            "loading {} messages from session (tree nodes: {})",
            session_state.messages.len(),
            session_state.message_nodes.len()
        );
        self.runtime.restore_conversation(
            session_state.message_nodes,
            session_state.active_path,
            session_state.next_node_id,
            session_state.messages,
        );
        self.runtime
            .set_tool_executions(session_state.tool_executions);

        {
            let state = self.app_state_mut();
            state.plan = session_state.plan.clone();
            state.session_config = session_state.config;
        }
        let session_config = self.app_state().session_config.clone();
        self.runtime
            .set_dialect(crate::tool_dialects::dialect_for(session_config.tool_syntax));
        if session_config.use_diff_blocks {
            self.enable_diff_blocks();
        } else {
            self.app_state_mut().session_config.use_diff_blocks = false;
            self.set_tool_scope(ToolScope::Agent);
            self.invalidate_system_message_cache();
        }
        self.runtime.normalize_loaded_message_history();
        {
            let state = self.app_state_mut();
            state.session_name = session_state.name;
            state.model_config = session_state.model_config;
            state.context_limit_override = None;
        }
        if let Some(model_config) = self.app_state().model_config.clone() {
            // Resolve the model identifier (provider/id) from the display name
            // This is used for system prompt selection based on model ID matching
            let model_hint = llm::provider_config::ConfigurationSystem::load()
                .ok()
                .and_then(|config| config.model_identifier(&model_config.model_name))
                .or_else(|| Some(model_config.model_name.clone()));
            self.set_model_hint(model_hint);
        }

        // Restore next_request_id from session, or calculate from existing messages for backward compatibility
        let next_request_id = session_state.next_request_id.unwrap_or_else(|| {
            self.runtime
                .message_history()
                .iter()
                .filter(|msg| matches!(msg.role, llm::MessageRole::Assistant))
                .count() as u64
                + 1
        });
        self.runtime.set_next_request_id(next_request_id);

        // Refresh project information from project manager (regenerates file trees and available_projects)
        self.init_projects()?;

        Ok(())
    }

    /// The restored plan state, for the caller to announce to the UI after a
    /// session load.
    pub fn plan(&self) -> &crate::types::PlanState {
        &self.app_state().plan
    }

    #[allow(dead_code)]
    pub fn init_project_context(&mut self) -> Result<()> {
        {
            let state = self.app_state_mut();
            // Initialize empty structures for multi-project support
            state.file_trees = std::collections::HashMap::new();
            state.available_projects = Vec::new();

            // Reset the initial project
            state.session_config.initial_project = String::new();
        }

        self.init_projects()
    }

    fn init_projects(&mut self) -> Result<()> {
        // Use effective_project_path: worktree path takes priority over init_path
        if let Some(path) = self
            .app_state()
            .session_config
            .effective_project_path()
            .cloned()
        {
            // Add as temporary project and get its name
            let project_name = self.project_manager.add_temporary_project(path)?;

            // Only set initial_project if not already set (first init).
            // When switching worktrees the project registration changes but
            // the sidebar grouping (initial_project) must stay stable.
            if self.app_state().session_config.initial_project.is_empty() {
                self.app_state_mut().session_config.initial_project = project_name.clone();
            }

            // Create initial file tree for this project
            let mut explorer = self
                .project_manager
                .get_explorer_for_project(&project_name)?;
            let tree = explorer.create_initial_tree(2)?; // Limited depth for initial tree

            // Store file tree as string for system prompt
            self.app_state_mut()
                .file_trees
                .insert(project_name, tree.to_string());
        }

        // Load all available projects
        let all_projects = self.project_manager.get_projects()?;
        for project_name in all_projects.keys() {
            if !self.app_state().available_projects.contains(project_name) {
                self.app_state_mut()
                    .available_projects
                    .push(project_name.clone());
            }
        }

        Ok(())
    }

    /// Start a new agent task
    #[cfg(test)]
    pub async fn start_with_task(&mut self, task: String) -> Result<()> {
        debug!("Starting agent with task: {}", task);

        self.init_project_context()?;

        self.runtime
            .restore_conversation(std::collections::BTreeMap::new(), Vec::new(), 1, Vec::new());
        use agent_core::AgentUi;
        self.ui_adapter
            .send_event(agent_core::AgentUiEvent::UserInputAppended {
                content: task.clone(),
                node_id: None, // Initial task message
            })
            .await?;

        // Create the initial user message
        self.append_message(Message::new_user(task.clone()))?;

        self.run_single_iteration().await
    }

    /// Invalidate the cached system message to force regeneration
    pub fn invalidate_system_message_cache(&mut self) {
        self.runtime.invalidate_system_message_cache();
    }

    /// Runs the iteration hooks over the rendered messages right before they
    /// are sent to the LLM (e.g. to inject system reminders).
    #[cfg(test)]
    pub(crate) fn shape_request_messages(&mut self, messages: Vec<Message>) -> Vec<Message> {
        self.runtime.shape_request_messages(messages)
    }

    /// Prepare messages for LLM request, dynamically rendering tool outputs.
    #[cfg(test)]
    pub fn render_tool_results_in_messages(&self) -> Vec<Message> {
        self.runtime.render_tool_results_in_messages()
    }

    #[cfg(test)]
    pub fn set_test_session_metadata(
        &mut self,
        session_id: String,
        model_config: SessionModelConfig,
    ) {
        self.ui_adapter.set_session_id(Some(session_id.clone()));
        self.runtime.set_session_id(Some(session_id));
        self.app_state_mut().model_config = Some(model_config);
    }

    #[cfg(test)]
    pub fn set_test_context_limit(&mut self, limit: u32) {
        self.app_state_mut().context_limit_override = Some(limit);
    }

    #[cfg(test)]
    pub fn message_history_for_tests(&self) -> &[Message] {
        self.runtime.message_history()
    }

    /// Static helper to update tool call in text (kept for tests; the live
    /// path runs inside the runtime).
    #[cfg(test)]
    pub fn update_tool_call_in_text_static(
        text: &str,
        updated_request: &crate::tools::ToolRequest,
        dialect: &dyn agent_core::ToolDialect,
        registry: &crate::tools::core::ToolRegistry,
    ) -> Result<String> {
        AgentRuntime::update_tool_call_in_text_static(text, updated_request, dialect, registry)
    }
}
