mod run;
#[cfg(test)]
mod tests;

use crate::agent::persistence::NoOpStatePersistence;
use crate::agent::{Agent, AgentComponents};
use crate::config::DefaultProjectManager;
use crate::persistence::SessionModelConfig;
use crate::session::SessionConfig;
use crate::tools::core::ToolScope;
use crate::ui::{ToolStatus, UiEvent, UserInterface};
use anyhow::Result;
use command_executor::{CommandExecutor, DefaultCommandExecutor, SandboxedCommandExecutor};
use llm::Message;
use sandbox::{SandboxContext, SandboxPolicy};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, atomic::AtomicBool, atomic::Ordering};
use tools_core::permissions::{PermissionMediator, ToolPermissions};

/// Cancellation registry keyed by the parent `spawn_agent` tool id.
#[derive(Default)]
pub struct SubAgentCancellationRegistry {
    flags: Mutex<HashMap<String, ChildCancellation>>,
}

#[derive(Clone)]
struct ChildCancellation {
    flag: Arc<AtomicBool>,
    token: tools_core::RunCancellation,
}

impl ChildCancellation {
    fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.token.cancel();
    }
}

struct ChildRegistration<'a> {
    registry: &'a SubAgentCancellationRegistry,
    tool_id: String,
    cancellation: ChildCancellation,
}

impl Drop for ChildRegistration<'_> {
    fn drop(&mut self) {
        let mut entries = self.registry.flags.lock().unwrap();
        // A delayed old task must not unregister a replacement with the same id.
        if entries
            .get(&self.tool_id)
            .is_some_and(|entry| entry.token.same_run(&self.cancellation.token))
        {
            entries.remove(&self.tool_id);
        }
    }
}

impl SubAgentCancellationRegistry {
    fn insert(&self, tool_id: String) -> ChildCancellation {
        let child = ChildCancellation {
            flag: Arc::new(AtomicBool::new(false)),
            token: tools_core::RunCancellation::default(),
        };
        if let Some(previous) = self.flags.lock().unwrap().insert(tool_id, child.clone()) {
            previous.cancel();
        }
        child
    }

    /// Compatibility flag for callers that observe cancellation synchronously.
    /// Use `cancel` to also wake asynchronous waiters.
    pub fn register(&self, tool_id: String) -> Arc<AtomicBool> {
        self.insert(tool_id).flag
    }

    fn register_run(&self, tool_id: &str) -> ChildRegistration<'_> {
        ChildRegistration {
            registry: self,
            tool_id: tool_id.to_string(),
            cancellation: self.insert(tool_id.to_string()),
        }
    }

    pub fn cancel(&self, tool_id: &str) -> bool {
        let flags = self.flags.lock().unwrap();
        if let Some(child) = flags.get(tool_id) {
            child.cancel();
            true
        } else {
            false
        }
    }

    pub fn unregister(&self, tool_id: &str) {
        let mut flags = self.flags.lock().unwrap();
        flags.remove(tool_id);
    }
}

/// Aggregated token usage for a sub-agent run.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SubAgentUsage {
    /// Total input tokens across all LLM requests in the sub-agent run
    pub input_tokens: u32,
    /// Total output tokens across all LLM requests in the sub-agent run
    pub output_tokens: u32,
    /// Total cache creation input tokens
    pub cache_creation_input_tokens: u32,
    /// Total cache read input tokens
    pub cache_read_input_tokens: u32,
    /// The model's context token limit (if known)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_limit: Option<u32>,
    /// Input tokens from the last LLM request (for context ratio computation)
    #[serde(default)]
    pub last_request_input_tokens: u32,
    /// Cache write (creation) tokens from the last LLM request
    #[serde(default)]
    pub last_request_cache_write_tokens: u32,
    /// Cache read tokens from the last LLM request (for context ratio computation)
    #[serde(default)]
    pub last_request_cache_read_tokens: u32,
    /// Output tokens from the last LLM request
    #[serde(default)]
    pub last_request_output_tokens: u32,
}

impl SubAgentUsage {
    /// Compute the context usage ratio (0.0..=1.0) for the last LLM request.
    ///
    /// This mirrors how the parent agent computes usage: all token categories
    /// of the last request occupy the context window. Input and cache-write
    /// tokens are both part of the prompt, cache-read tokens were replayed from
    /// the cache into the prompt, and the last request's output tokens become
    /// part of the prompt on the next request.
    pub fn context_ratio(&self) -> Option<f32> {
        let limit = self.context_limit?;
        if limit == 0 {
            return None;
        }
        let used = self
            .last_request_input_tokens
            .saturating_add(self.last_request_cache_write_tokens)
            .saturating_add(self.last_request_cache_read_tokens)
            .saturating_add(self.last_request_output_tokens);
        if used > 0 {
            Some(used as f32 / limit as f32)
        } else {
            None
        }
    }
}

/// Result from a sub-agent run, containing both the answer and UI output
#[derive(Debug, Clone)]
pub struct SubAgentResult {
    /// The plain text answer for LLM context
    pub answer: String,
    /// The JSON output for UI display (tools list + response)
    pub ui_output: String,
}

/// A failed child still has useful structured evidence. The spawn tool persists
/// this output instead of replacing it with an unstructured error string.
#[derive(Debug)]
pub struct SubAgentFailure {
    pub message: String,
    pub ui_output: String,
}

impl std::fmt::Display for SubAgentFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for SubAgentFailure {}

/// Execution mode for a sub-agent, selected by the `spawn_agent` tool input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubAgentMode {
    /// Only read-only tools are available.
    ReadOnly,
    /// The sub-agent may also edit files and run commands.
    Default,
}

/// Runs sub-agents with isolated history and streams a compact progress view into the parent tool UI.
#[async_trait::async_trait]
pub trait SubAgentRunner: Send + Sync {
    async fn run(
        &self,
        parent_tool_id: &str,
        instructions: String,
        mode: SubAgentMode,
        require_file_references: bool,
    ) -> Result<SubAgentResult>;
}

pub struct DefaultSubAgentRunner {
    model_name: String,
    session_config: SessionConfig,
    sandbox_policy: SandboxPolicy,
    sandbox_context: Arc<SandboxContext>,
    cancellation_registry: Arc<SubAgentCancellationRegistry>,
    /// The parent UI to stream progress updates to.
    ui: Arc<dyn UserInterface>,
    /// Optional permission handler for sub-agent tool invocations.
    permission_handler: Option<Arc<dyn PermissionMediator>>,
    /// Permission tier and grants shared with the parent session.
    permissions: ToolPermissions,
    /// The tool registry sub-agents run with (shared with the parent agent).
    tool_registry: Arc<crate::tools::core::ToolRegistry>,
    /// Read-only view of the session store, so sub-agents get the session
    /// introspection tools (`search_sessions`, `get_session_content`) — this
    /// is what lets the parent split up work across the session archive.
    session_source: Option<Arc<dyn crate::session_query::SessionSource>>,
    /// Hook factory sub-agents run with (shared with the parent agent);
    /// `None` uses code-assistant's default hooks.
    hooks_factory: Option<agent_core::hooks::HookRegistryFactory>,
    llm_client_factory: Option<crate::session::service::LlmClientFactory>,
    project_manager_factory: crate::session::service::ProjectManagerFactory,
    parent_cancellation: tools_core::RunCancellation,
}

impl DefaultSubAgentRunner {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        model_name: String,
        session_config: SessionConfig,
        sandbox_context: Arc<SandboxContext>,
        cancellation_registry: Arc<SubAgentCancellationRegistry>,
        ui: Arc<dyn UserInterface>,
        permission_handler: Option<Arc<dyn PermissionMediator>>,
        permissions: ToolPermissions,
        tool_registry: Arc<crate::tools::core::ToolRegistry>,
        session_source: Option<Arc<dyn crate::session_query::SessionSource>>,
        hooks_factory: Option<agent_core::hooks::HookRegistryFactory>,
    ) -> Self {
        let sandbox_policy = session_config.sandbox_policy.clone();
        Self {
            model_name,
            session_config,
            sandbox_policy,
            sandbox_context,
            cancellation_registry,
            ui,
            permission_handler,
            permissions,
            tool_registry,
            session_source,
            hooks_factory,
            llm_client_factory: None,
            project_manager_factory: Arc::new(|| Box::new(DefaultProjectManager::new())),
            parent_cancellation: tools_core::RunCancellation::default(),
        }
    }

    pub fn with_llm_client_factory(
        mut self,
        factory: crate::session::service::LlmClientFactory,
    ) -> Self {
        self.llm_client_factory = Some(factory);
        self
    }

    pub fn with_project_manager_factory(
        mut self,
        factory: crate::session::service::ProjectManagerFactory,
    ) -> Self {
        self.project_manager_factory = factory;
        self
    }

    pub fn with_parent_cancellation(mut self, cancellation: tools_core::RunCancellation) -> Self {
        self.parent_cancellation = cancellation;
        self
    }

    fn build_sub_agent_ui(
        &self,
        parent_ui: Arc<dyn UserInterface>,
        parent_tool_id: String,
        cancelled: Arc<AtomicBool>,
    ) -> Arc<SubAgentUiAdapter> {
        Arc::new(SubAgentUiAdapter::new(
            parent_ui,
            parent_tool_id,
            cancelled,
            self.tool_registry.clone(),
        ))
    }

    async fn build_agent(
        &self,
        parent_tool_id: &str,
        ui: Arc<dyn UserInterface>,
        permission_handler: Option<Arc<dyn PermissionMediator>>,
    ) -> Result<Agent> {
        // Create a fresh LLM provider (avoid requiring Clone).
        let llm_provider = match &self.llm_client_factory {
            Some(factory) => {
                let factory = factory.clone();
                let model = self.model_name.clone();
                tokio::task::spawn_blocking(move || factory(&model)).await??
            }
            None => {
                llm::factory::create_llm_client_from_model(&self.model_name, None, false, None)
                    .await?
            }
        };

        // Create a fresh project manager, copying init_path if set.
        let project_manager: Arc<dyn crate::config::ProjectManager> =
            Arc::from((self.project_manager_factory)());
        if let Some(path) = self.session_config.effective_project_path().cloned() {
            let _ = project_manager.add_temporary_project(path);
        }

        let command_executor: Box<dyn CommandExecutor> = {
            let base: Box<dyn CommandExecutor> = Box::new(DefaultCommandExecutor);
            if self.sandbox_policy.requires_restrictions() {
                Box::new(SandboxedCommandExecutor::new(
                    base,
                    self.sandbox_policy.clone(),
                    Some(self.sandbox_context.clone()),
                    Some(format!("sub-agent:{parent_tool_id}")),
                ))
            } else {
                base
            }
        };

        let components = AgentComponents {
            llm_provider,
            project_manager,
            command_executor: Arc::from(command_executor),
            ui,
            state_persistence: Box::new(NoOpStatePersistence),
            permission_handler,
            permissions: self.permissions.clone(),
            tool_registry: self.tool_registry.clone(),
            sub_agent_runner: None,
            // Sub-agents run to completion inside the parent's turn; a
            // wakeup for "their" session would wake the parent instead.
            wakeups: None,
            // Sub-agents get their own registry: dropping it when the
            // sub-agent finishes terminates any PTY sessions it left behind.
            pty_sessions: Some(Arc::new(pty_session::PtySessionManager::default())),
            browser_sessions: Some(Arc::new(web::BrowserSessionManager::default())),
            terminal_interrupts: Some(Arc::new(crate::tools::TerminalInterrupts::default())),
            // Read-only session archive access, so the introspection tools
            // work in sub-agents (parent shares its store).
            session_source: self.session_source.clone(),
            hooks_factory: self.hooks_factory.clone(),
        };

        let mut agent = Agent::new(components, self.session_config.clone());

        // Configure for sub-agent use.
        agent.set_tool_scope(tool_scope_for_subagent());

        // Ensure it uses the same model name for prompt selection.
        agent.set_session_model_config(SessionModelConfig::new(self.model_name.clone()));

        // Provide a stable session id so UI components that key off it don't break.
        agent.set_session_identity(format!("sub-agent:{parent_tool_id}"), String::new());

        // Initialize project trees, etc.
        agent.init_project_context()?;

        Ok(agent)
    }
}

fn tool_scope_for_subagent() -> ToolScope {
    // default; actual scope is set by caller via Agent::set_tool_scope
    ToolScope::SubAgentReadOnly
}

/// Compute aggregated token usage from a sub-agent's message history.
fn compute_sub_agent_usage(messages: &[Message], model_name: &str) -> SubAgentUsage {
    let mut total = SubAgentUsage::default();

    for message in messages {
        if let Some(usage) = &message.usage {
            total.input_tokens += usage.input_tokens;
            total.output_tokens += usage.output_tokens;
            total.cache_creation_input_tokens += usage.cache_creation_input_tokens;
            total.cache_read_input_tokens += usage.cache_read_input_tokens;

            // Track last assistant message's usage for context ratio
            if matches!(message.role, llm::MessageRole::Assistant) {
                total.last_request_input_tokens = usage.input_tokens;
                total.last_request_cache_write_tokens = usage.cache_creation_input_tokens;
                total.last_request_cache_read_tokens = usage.cache_read_input_tokens;
                total.last_request_output_tokens = usage.output_tokens;
            }
        }
    }

    // Resolve the model's context token limit
    if let Ok(config_system) = llm::provider_config::ConfigurationSystem::load()
        && let Some(model) = config_system.get_model(model_name)
    {
        total.context_limit = Some(model.context_token_limit);
    }

    total
}

fn extract_last_assistant_text(messages: &[Message]) -> Option<String> {
    messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, llm::MessageRole::Assistant))
        .map(extract_text_from_message)
}

/// Extract just the text content from a message, ignoring tool calls and other blocks
fn extract_text_from_message(message: &Message) -> String {
    match &message.content {
        llm::MessageContent::Text(text) => text.clone(),
        llm::MessageContent::Structured(blocks) => {
            let mut text_parts = Vec::new();
            for block in blocks {
                match block {
                    llm::ContentBlock::Text { text, .. } => {
                        text_parts.push(text.as_str());
                    }
                    _ => {
                        // Skip tool uses, thinking, tool results, images, etc.
                    }
                }
            }
            text_parts.join("\n\n")
        }
    }
}

fn has_file_references_with_line_ranges(text: &str) -> bool {
    // Very lightweight heuristic:
    // - backticked `path:10-20` OR raw path:10-20
    // - accept common extensions.
    // Note: Rust regex doesn't support backreferences, so we use alternation instead.
    let pattern = r"(?m)(`[\w./-]+\.(rs|ts|tsx|js|jsx|py|go|java|kt|swift|c|cc|cpp|h|hpp|md|toml|json|yaml|yml):(\d+)(-\d+)?`|[\w./-]+\.(rs|ts|tsx|js|jsx|py|go|java|kt|swift|c|cc|cpp|h|hpp|md|toml|json|yaml|yml):(\d+)(-\d+)?)";
    regex::Regex::new(pattern)
        .map(|r| r.is_match(text))
        .unwrap_or(false)
}

/// Structured representation of a sub-agent tool call for UI display and persistence.
/// This is serialized to JSON as the spawn_agent tool output.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SubAgentToolCall {
    pub name: String,
    pub status: SubAgentToolStatus,
    /// Human-readable title generated from tool's title_template and parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Status message (e.g., "Successfully loaded 2 file(s)")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Parameters collected during streaming (used to generate title)
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub parameters: std::collections::HashMap<String, String>,
}

/// Status of a sub-agent tool call
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubAgentToolStatus {
    Running,
    Success,
    Error,
}

impl From<ToolStatus> for SubAgentToolStatus {
    fn from(status: ToolStatus) -> Self {
        match status {
            ToolStatus::Pending | ToolStatus::Running => SubAgentToolStatus::Running,
            ToolStatus::Success => SubAgentToolStatus::Success,
            ToolStatus::Error => SubAgentToolStatus::Error,
        }
    }
}

/// Current activity state of the sub-agent
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubAgentActivity {
    /// Waiting for LLM to start streaming
    WaitingForLlm,
    /// LLM is streaming response
    Streaming,
    /// Executing tools
    ExecutingTools,
    /// Completed successfully
    Completed,
    /// Was cancelled
    Cancelled,
    /// Encountered an error
    Failed,
}

/// Structured output for spawn_agent tool, serialized as JSON
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SubAgentOutput {
    pub tools: Vec<SubAgentToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity: Option<SubAgentActivity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Final response from the sub-agent (set when completed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    /// Aggregated token usage from the sub-agent's LLM requests
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<SubAgentUsage>,
}

impl SubAgentOutput {
    pub fn new() -> Self {
        Self {
            tools: Vec::new(),
            activity: Some(SubAgentActivity::WaitingForLlm),
            cancelled: None,
            error: None,
            response: None,
            usage: None,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    pub fn from_json(json: &str) -> Option<Self> {
        serde_json::from_str(json).ok()
    }
}

impl Default for SubAgentOutput {
    fn default() -> Self {
        Self::new()
    }
}

/// A minimal UI adapter that captures sub-agent activity as structured data and streams it
/// into the parent `spawn_agent` tool block.
struct SubAgentUiAdapter {
    parent: Arc<dyn UserInterface>,
    parent_tool_id: String,
    cancelled: Arc<AtomicBool>,
    output: Mutex<SubAgentOutput>,
    /// Map from tool_id to index in output.tools for fast lookup
    tool_id_to_index: Mutex<std::collections::HashMap<String, usize>>,
    /// Registry for tool title templates.
    tool_registry: Arc<crate::tools::core::ToolRegistry>,
}

impl SubAgentUiAdapter {
    fn new(
        parent: Arc<dyn UserInterface>,
        parent_tool_id: String,
        cancelled: Arc<AtomicBool>,
        tool_registry: Arc<crate::tools::core::ToolRegistry>,
    ) -> Self {
        Self {
            parent,
            parent_tool_id,
            cancelled,
            output: Mutex::new(SubAgentOutput::new()),
            tool_id_to_index: Mutex::new(std::collections::HashMap::new()),
            tool_registry,
        }
    }

    async fn send_output_update(&self) {
        let (json, tool_count, activity, status, message) = {
            let output = self.output.lock().unwrap();
            let (status, message) = match output.activity {
                Some(SubAgentActivity::Completed) => (ToolStatus::Success, "Sub-agent completed"),
                Some(SubAgentActivity::Failed) => (ToolStatus::Error, "Sub-agent failed"),
                Some(SubAgentActivity::Cancelled) => (ToolStatus::Error, "Sub-agent cancelled"),
                _ => (ToolStatus::Running, "Sub-agent running"),
            };
            (
                output.to_json(),
                output.tools.len(),
                output.activity,
                status,
                message,
            )
        };

        tracing::debug!(
            "SubAgentUiAdapter: Sending output update - {} tools, activity={:?}, json_len={}",
            tool_count,
            activity,
            json.len()
        );

        let _ = self
            .parent
            .send_event(UiEvent::UpdateToolStatus {
                tool_id: self.parent_tool_id.clone(),
                status,
                message: Some(message.to_string()),
                output: Some(json),
                styled_output: None,
                duration_seconds: None,
                images: vec![],
            })
            .await;
    }

    fn add_tool_start(&self, name: &str, id: &str) {
        let mut output = self.output.lock().unwrap();
        let mut id_map = self.tool_id_to_index.lock().unwrap();

        // Check if tool already exists (avoid duplicates)
        if id_map.contains_key(id) {
            tracing::debug!(
                "SubAgentUiAdapter: Tool already exists, skipping add: {} ({})",
                name,
                id
            );
            return;
        }

        // Add new tool as running
        let index = output.tools.len();
        output.tools.push(SubAgentToolCall {
            name: name.to_string(),
            status: SubAgentToolStatus::Running,
            title: None,
            message: None,
            parameters: std::collections::HashMap::new(),
        });
        id_map.insert(id.to_string(), index);
        tracing::debug!(
            "SubAgentUiAdapter: Added tool {} ({}) at index {}, total tools: {}",
            name,
            id,
            index,
            output.tools.len()
        );
    }

    fn add_tool_parameter(&self, tool_id: &str, name: &str, value: &str) {
        let mut output = self.output.lock().unwrap();
        let id_map = self.tool_id_to_index.lock().unwrap();

        if let Some(&index) = id_map.get(tool_id)
            && let Some(tool) = output.tools.get_mut(index)
        {
            // Append to existing parameter value (streaming may send chunks)
            let entry = tool.parameters.entry(name.to_string()).or_default();
            entry.push_str(value);

            // Update title from template using collected parameters
            if let Some(new_title) = crate::tools::core::generate_tool_title(
                &tool.name,
                &tool.parameters,
                self.tool_registry.as_ref(),
            ) {
                tool.title = Some(new_title);
            }
        }
    }

    fn update_tool_status(&self, tool_id: &str, status: ToolStatus, message: Option<String>) {
        let mut output = self.output.lock().unwrap();
        let mut id_map = self.tool_id_to_index.lock().unwrap();

        // Find tool by id and update its status
        if let Some(&index) = id_map.get(tool_id) {
            if let Some(tool) = output.tools.get_mut(index) {
                tracing::debug!(
                    "SubAgentUiAdapter: Updating tool {} status to {:?}",
                    tool.name,
                    status
                );
                tool.status = status.into();
                tool.message = message;
            }
        } else {
            // Tool not found - this can happen if UpdateToolStatus arrives before ToolName
            // In this case, we should add the tool
            tracing::warn!(
                "SubAgentUiAdapter: UpdateToolStatus for unknown tool_id={}, status={:?}. Adding placeholder.",
                tool_id,
                status
            );
            let index = output.tools.len();
            output.tools.push(SubAgentToolCall {
                name: format!("tool_{}", tool_id.chars().take(8).collect::<String>()),
                status: status.into(),
                title: None,
                message,
                parameters: std::collections::HashMap::new(),
            });
            id_map.insert(tool_id.to_string(), index);
        }
    }

    fn set_cancelled(&self) {
        let mut output = self.output.lock().unwrap();
        output.cancelled = Some(true);
    }

    fn set_error(&self, error: String) {
        let mut output = self.output.lock().unwrap();
        output.error = Some(error);
        output.activity = Some(SubAgentActivity::Failed);
        for tool in &mut output.tools {
            if tool.status == SubAgentToolStatus::Running {
                tool.status = SubAgentToolStatus::Error;
                tool.message = Some("Sub-agent ended without a recorded outcome for this tool; effects are unknown.".into());
            }
        }
    }

    fn set_activity(&self, activity: SubAgentActivity) {
        let mut output = self.output.lock().unwrap();
        output.activity = Some(activity);
    }

    fn set_response(&self, response: String) {
        let mut output = self.output.lock().unwrap();
        output.response = Some(response);
        output.activity = Some(SubAgentActivity::Completed);
        output.error = None;
        output.cancelled = None;
    }

    fn set_usage(&self, usage: SubAgentUsage) {
        let mut output = self.output.lock().unwrap();
        output.usage = Some(usage);
    }

    /// Get the final JSON output including response
    fn get_final_output(&self) -> String {
        let output = self.output.lock().unwrap();
        output.to_json()
    }
}

#[async_trait::async_trait]
impl UserInterface for SubAgentUiAdapter {
    async fn send_event(&self, event: UiEvent) -> Result<(), crate::ui::UIError> {
        match &event {
            UiEvent::UpdateToolStatus {
                tool_id,
                status,
                message,
                ..
            } => {
                tracing::debug!(
                    "SubAgentUiAdapter: UpdateToolStatus event - tool_id={}, status={:?}",
                    tool_id,
                    status
                );
                self.update_tool_status(tool_id, *status, message.clone());
                // If a tool is running, we're executing tools
                if *status == ToolStatus::Running {
                    self.set_activity(SubAgentActivity::ExecutingTools);
                }
                self.send_output_update().await;
            }

            UiEvent::StreamingStarted { .. } => {
                tracing::debug!("SubAgentUiAdapter: StreamingStarted");
                self.set_activity(SubAgentActivity::Streaming);
                self.send_output_update().await;
            }

            UiEvent::StreamingStopped {
                cancelled, error, ..
            } => {
                tracing::debug!(
                    "SubAgentUiAdapter: StreamingStopped - cancelled={}, error={:?}",
                    cancelled,
                    error
                );
                if *cancelled {
                    self.set_cancelled();
                    self.set_activity(SubAgentActivity::Cancelled);
                } else if let Some(err) = error {
                    self.set_error(err.clone());
                    // activity already set to Failed in set_error
                } else {
                    // Streaming stopped normally - will likely execute tools or complete
                    // The activity will be updated by tool execution or completion
                }
                self.send_output_update().await;
            }
            _ => {
                // Ignore other events; they belong to the sub-agent's isolated transcript.
            }
        }

        Ok(())
    }

    fn display_fragment(
        &self,
        fragment: &crate::ui::DisplayFragment,
    ) -> Result<(), crate::ui::UIError> {
        use crate::ui::DisplayFragment;

        match fragment {
            DisplayFragment::ToolName { name, id, .. } => {
                // A sub-agent tool is starting - capture it in our internal state.
                // This is called during LLM streaming when the tool name is parsed.
                //
                // Note: We don't notify the parent UI here because display_fragment() is
                // synchronous. The parent UI will be notified when runner.rs sends
                // UiEvent::UpdateToolStatus with Running status just before tool execution
                // starts. At that point, send_event() calls send_output_update() which
                // forwards our accumulated state (including this tool) to the parent.
                tracing::debug!(
                    "SubAgentUiAdapter: ToolName fragment received: {} ({})",
                    name,
                    id
                );
                self.add_tool_start(name, id);
            }
            DisplayFragment::ToolParameter {
                name,
                value,
                tool_id,
            } => {
                // Capture parameters to generate tool titles
                self.add_tool_parameter(tool_id, name, value);
            }
            _ => {
                // Ignore other fragments (thinking, images, etc.)
                // They belong to the sub-agent's isolated transcript
            }
        }

        Ok(())
    }

    fn should_streaming_continue(&self) -> bool {
        !self.cancelled.load(Ordering::SeqCst) && self.parent.should_streaming_continue()
    }

    fn notify_rate_limit(&self, _seconds_remaining: u64) {}

    fn clear_rate_limit(&self) {}
}
