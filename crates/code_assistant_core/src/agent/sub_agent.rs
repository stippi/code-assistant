mod run;
pub mod running;
#[cfg(test)]
mod tests;

pub use running::{RunningSubAgentInfo, RunningSubAgents, highest_sub_agent_id};

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
use std::sync::{Arc, Mutex};
use tools_core::permissions::{PermissionMediator, ToolPermissions};

/// Cancellation tokens of the running sub-agents, keyed by the parent
/// `spawn_agent` tool id, so a child can be cancelled from the UI.
#[derive(Default)]
pub struct SubAgentCancellationRegistry {
    children: Mutex<HashMap<String, tools_core::RunCancellation>>,
}

/// A child's entry in the registry; removed when the run ends.
struct ChildRegistration<'a> {
    registry: &'a SubAgentCancellationRegistry,
    tool_id: String,
    token: tools_core::RunCancellation,
}

impl Drop for ChildRegistration<'_> {
    fn drop(&mut self) {
        let mut children = self.registry.children.lock().unwrap();
        // A delayed old task must not unregister a replacement with the same id.
        if children
            .get(&self.tool_id)
            .is_some_and(|token| token.same_run(&self.token))
        {
            children.remove(&self.tool_id);
        }
    }
}

impl SubAgentCancellationRegistry {
    /// Register a child of `parent`. An earlier child with the same id is
    /// cancelled and replaced.
    fn register_run(
        &self,
        tool_id: &str,
        parent: &tools_core::RunCancellation,
    ) -> ChildRegistration<'_> {
        let token = parent.child();
        if let Some(previous) = self
            .children
            .lock()
            .unwrap()
            .insert(tool_id.to_string(), token.clone())
        {
            previous.cancel();
        }
        ChildRegistration {
            registry: self,
            tool_id: tool_id.to_string(),
            token,
        }
    }

    /// Cancel the child running for `tool_id`. Returns `false` if none is.
    pub fn cancel(&self, tool_id: &str) -> bool {
        match self.children.lock().unwrap().get(tool_id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
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

/// A background sub-agent that was started: the parent goes on without
/// waiting for it.
#[derive(Debug, Clone)]
pub struct BackgroundStart {
    /// Short per-session id the model and the frontends refer to it by.
    pub agent_id: u64,
    /// The card state at start, for the `spawn_agent` result.
    pub ui_output: String,
}

/// How a background sub-agent ended.
#[derive(Debug, Clone)]
pub struct FinishedSubAgent {
    pub agent_id: u64,
    /// The `spawn_agent` call that started it.
    pub tool_id: String,
    pub outcome: FinishedOutcome,
    /// The final card state.
    pub ui_output: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishedOutcome {
    Completed { answer: String },
    Failed { error: String },
    Cancelled { message: String },
}

impl FinishedSubAgent {
    /// The message delivered to the parent session.
    pub fn message(&self) -> String {
        match &self.outcome {
            FinishedOutcome::Completed { answer } => {
                format!(
                    "{SUB_AGENT_FRAME} #{}] finished:\n\n{answer}",
                    self.agent_id
                )
            }
            FinishedOutcome::Failed { error } => {
                format!("{SUB_AGENT_FRAME} #{}] failed: {error}", self.agent_id)
            }
            FinishedOutcome::Cancelled { message } => {
                format!("{SUB_AGENT_FRAME} #{}] cancelled: {message}", self.agent_id)
            }
        }
    }
}

/// Start of the frame of a delivered background result, so the agent and
/// the transcript can tell it from a human message: `[sub-agent #3] finished: …`.
pub const SUB_AGENT_FRAME: &str = "[sub-agent";

/// A delivered background result, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSubAgentMessage<'a> {
    pub agent_id: u64,
    /// `finished`, `failed` or `cancelled`.
    pub outcome: &'a str,
    /// The answer, or what went wrong.
    pub body: &'a str,
}

/// Parse a message rendered by [`FinishedSubAgent::message`]; `None` for any
/// other message.
pub fn parse_sub_agent_message(message: &str) -> Option<ParsedSubAgentMessage<'_>> {
    let rest = message.strip_prefix(SUB_AGENT_FRAME)?.strip_prefix(" #")?;
    let (id, rest) = rest.split_once("] ")?;
    let (outcome, body) = rest.split_once(':')?;
    if !matches!(outcome, "finished" | "failed" | "cancelled") {
        return None;
    }
    Some(ParsedSubAgentMessage {
        agent_id: id.parse().ok()?,
        outcome,
        body: body.trim(),
    })
}

/// Parse a message holding one or more delivered results, as queued results
/// arrive together (frontends may join their blocks with blank lines);
/// `None` unless all of it is results.
pub fn parse_sub_agent_messages(message: &str) -> Option<Vec<ParsedSubAgentMessage<'_>>> {
    let separator = format!("\n\n{SUB_AGENT_FRAME} #");
    let mut starts = vec![0];
    starts.extend(
        message
            .match_indices(&separator)
            .map(|(i, _)| i + 2)
            .filter(|&i| parse_sub_agent_message(&message[i..]).is_some()),
    );
    let mut parsed = Vec::new();
    for (n, &start) in starts.iter().enumerate() {
        let end = starts.get(n + 1).map_or(message.len(), |&next| next);
        parsed.push(parse_sub_agent_message(message[start..end].trim_end())?);
    }
    Some(parsed)
}

/// Receives the results of background sub-agents. The production
/// implementation is [`crate::session::SessionService`], which records the
/// result and delivers it into the parent session; tests substitute a
/// recorder.
#[async_trait::async_trait]
pub trait SubAgentSink: Send + Sync + 'static {
    async fn finished(&self, session_id: &str, finished: FinishedSubAgent);
}

/// What a runner needs to start sub-agents that outlive the parent's turn.
#[derive(Clone)]
pub struct BackgroundSupport {
    pub session_id: String,
    /// Publishes into the session without belonging to a turn.
    pub ui: Arc<dyn UserInterface>,
    /// Cancelled when the session goes away, not when a turn stops.
    pub cancellation: tools_core::RunCancellation,
    pub sink: Arc<dyn SubAgentSink>,
}

/// Where a sub-agent's conversation is kept, so it can be read like a
/// session.
pub struct SubAgentTranscript {
    pub session_id: String,
    /// Receives the sub-agent's checkpoints.
    pub persistence: Box<dyn crate::agent::persistence::AgentStatePersistence>,
    /// Receives everything the sub-agent shows.
    pub ui: Arc<dyn UserInterface>,
}

/// Creates the transcripts of a session's sub-agents (see
/// [`crate::session::sub_agent_transcripts`]).
pub trait SubAgentTranscripts: Send + Sync {
    fn create(&self, agent_id: u64, title: &str) -> Result<SubAgentTranscript>;
}

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

    /// Start a sub-agent without waiting for it; its result is delivered to
    /// the parent session when it ends.
    async fn start_background(
        &self,
        _parent_tool_id: &str,
        _instructions: String,
        _mode: SubAgentMode,
        _require_file_references: bool,
    ) -> Result<BackgroundStart> {
        anyhow::bail!(
            "Background sub-agents are not available here; spawn it without `background`."
        )
    }
}

#[derive(Clone)]
pub struct DefaultSubAgentRunner {
    /// The parent session's model and reasoning effort.
    model: SessionModelConfig,
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
    /// Where a sub-agent's browsers are listed while it runs, so the
    /// session's browser panel can show them.
    session_browsers: Option<Arc<crate::session::browsers::SessionBrowsers>>,
    /// Where running sub-agents are listed, with the session they belong to.
    running: Option<(String, Arc<RunningSubAgents>)>,
    background: Option<BackgroundSupport>,
    transcripts: Option<Arc<dyn SubAgentTranscripts>>,
}

impl DefaultSubAgentRunner {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        model: SessionModelConfig,
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
            model,
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
            session_browsers: None,
            running: None,
            background: None,
            transcripts: None,
        }
    }

    /// Keep each listed sub-agent's conversation as a transcript.
    pub fn with_transcripts(mut self, transcripts: Option<Arc<dyn SubAgentTranscripts>>) -> Self {
        self.transcripts = transcripts;
        self
    }

    /// List running sub-agents in `running`, published for `session_id`.
    pub fn with_running(mut self, session_id: String, running: Arc<RunningSubAgents>) -> Self {
        self.running = Some((session_id, running));
        self
    }

    /// List running sub-agents on the session `instance`, if there is one.
    pub fn with_running_from(
        self,
        instance: Option<&crate::session::instance::SessionInstance>,
        session_id: &str,
    ) -> Self {
        match instance {
            Some(instance) => {
                self.with_running(session_id.to_string(), instance.running_sub_agents.clone())
            }
            None => self,
        }
    }

    pub fn with_background(mut self, background: Option<BackgroundSupport>) -> Self {
        self.background = background;
        self
    }

    pub fn with_llm_client_factory(
        mut self,
        factory: crate::session::service::LlmClientFactory,
    ) -> Self {
        self.llm_client_factory = Some(factory);
        self
    }

    /// Use `factory` for the sub-agents' LLM clients when there is one.
    pub fn with_llm_client_factory_if(
        mut self,
        factory: Option<crate::session::service::LlmClientFactory>,
    ) -> Self {
        if factory.is_some() {
            self.llm_client_factory = factory;
        }
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

    pub fn with_session_browsers(
        mut self,
        browsers: Option<Arc<crate::session::browsers::SessionBrowsers>>,
    ) -> Self {
        self.session_browsers = browsers;
        self
    }

    fn build_sub_agent_ui(
        &self,
        parent_ui: Arc<dyn UserInterface>,
        parent_tool_id: String,
        cancellation: tools_core::RunCancellation,
        listing: Option<SubAgentListing>,
    ) -> Arc<SubAgentUiAdapter> {
        Arc::new(SubAgentUiAdapter::new(
            parent_ui,
            parent_tool_id,
            cancellation,
            self.tool_registry.clone(),
            listing,
        ))
    }

    async fn build_agent(
        &self,
        parent_tool_id: &str,
        ui: Arc<dyn UserInterface>,
        permission_handler: Option<Arc<dyn PermissionMediator>>,
        browser_sessions: Arc<web::BrowserSessionManager>,
        transcript: Option<(
            String,
            Box<dyn crate::agent::persistence::AgentStatePersistence>,
        )>,
    ) -> Result<Agent> {
        let (session_id, state_persistence): (
            String,
            Box<dyn crate::agent::persistence::AgentStatePersistence>,
        ) = match transcript {
            Some((session_id, persistence)) => (session_id, persistence),
            None => (
                format!("sub-agent:{parent_tool_id}"),
                Box::new(NoOpStatePersistence),
            ),
        };
        // Create a fresh LLM provider (avoid requiring Clone).
        let llm_provider = match &self.llm_client_factory {
            Some(factory) => {
                let factory = factory.clone();
                let model = self.model.model_name.clone();
                tokio::task::spawn_blocking(move || factory(&model)).await??
            }
            None => {
                llm::factory::create_llm_client_from_model(
                    &self.model.model_name,
                    self.model.effort,
                    None,
                    false,
                    None,
                )
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
            state_persistence,
            permission_handler,
            permissions: self.permissions.clone(),
            tool_registry: self.tool_registry.clone(),
            sub_agent_runner: None,
            // Sub-agents run to completion inside the parent's turn; a
            // wakeup for "their" session would wake the parent instead.
            wakeups: None,
            // Sub-agents get their own registries: dropping them when the
            // sub-agent finishes terminates any PTY sessions and browsers it
            // left behind.
            pty_sessions: Some(Arc::new(pty_session::PtySessionManager::default())),
            browser_sessions: Some(browser_sessions),
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
        agent.set_session_model_config(self.model.clone());

        // The transcript's id, or a stable one so UI components that key off
        // it don't break.
        agent.set_session_identity(session_id, String::new());

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
    total.context_limit = llm::provider_config::ConfigurationSystem::load()
        .ok()
        .and_then(|config| config.context_limit(model_name));

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
    /// The sub-agent's short per-session id (`#3`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<u64>,
    /// Started in the background: the parent did not wait for it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub background: bool,
    /// The session holding the sub-agent's transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
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
            agent_id: None,
            background: false,
            session_id: None,
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

/// Where a sub-agent is listed while it runs.
struct SubAgentListing {
    session_id: String,
    running: Arc<RunningSubAgents>,
    background: bool,
}

impl SubAgentListing {
    async fn publish(&self, ui: &dyn UserInterface) {
        let _ = ui
            .send_event(UiEvent::UpdateRunningSubAgents {
                session_id: self.session_id.clone(),
                agents: self.running.list(),
            })
            .await;
    }
}

/// A minimal UI adapter that captures sub-agent activity as structured data and streams it
/// into the parent `spawn_agent` tool block.
struct SubAgentUiAdapter {
    parent: Arc<dyn UserInterface>,
    parent_tool_id: String,
    cancellation: tools_core::RunCancellation,
    listing: Option<SubAgentListing>,
    /// The sub-agent's own transcript, which sees everything.
    transcript: Mutex<Option<Arc<dyn UserInterface>>>,
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
        cancellation: tools_core::RunCancellation,
        tool_registry: Arc<crate::tools::core::ToolRegistry>,
        listing: Option<SubAgentListing>,
    ) -> Self {
        Self {
            parent,
            parent_tool_id,
            cancellation,
            listing,
            transcript: Mutex::new(None),
            output: Mutex::new(SubAgentOutput::new()),
            tool_id_to_index: Mutex::new(std::collections::HashMap::new()),
            tool_registry,
        }
    }

    fn set_identity(&self, agent_id: u64, background: bool) {
        let mut output = self.output.lock().unwrap();
        output.agent_id = Some(agent_id);
        output.background = background;
    }

    fn set_transcript(&self, session_id: String, ui: Arc<dyn UserInterface>) {
        self.output.lock().unwrap().session_id = Some(session_id);
        *self.transcript.lock().unwrap() = Some(ui);
    }

    fn transcript(&self) -> Option<Arc<dyn UserInterface>> {
        self.transcript.lock().unwrap().clone()
    }

    async fn send_output_update(&self) {
        let (json, tool_count, activity, status, message, listing_changed) = {
            let output = self.output.lock().unwrap();
            let listing_changed = self
                .listing
                .as_ref()
                .is_some_and(|l| l.running.update(&self.parent_tool_id, &output));
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
                listing_changed,
            )
        };

        tracing::debug!(
            "SubAgentUiAdapter: Sending output update - {} tools, activity={:?}, json_len={}",
            tool_count,
            activity,
            json.len()
        );

        if let Some(listing) = &self.listing
            && listing.background
        {
            listing.running.set_live_result(
                &self.parent_tool_id,
                crate::ui::ui_events::ToolResultData {
                    tool_id: self.parent_tool_id.clone(),
                    status,
                    message: Some(message.to_string()),
                    output: Some(json.clone()),
                    styled_output: None,
                    duration_seconds: None,
                    images: vec![],
                    output_deferred: false,
                },
            );
        }

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

        if listing_changed && let Some(listing) = &self.listing {
            listing.publish(self.parent.as_ref()).await;
        }
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
        if let Some(transcript) = self.transcript() {
            let _ = transcript.send_event(event.clone()).await;
        }
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

        if let Some(transcript) = self.transcript() {
            let _ = transcript.display_fragment(fragment);
        }
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
        !self.cancellation.is_cancelled() && self.parent.should_streaming_continue()
    }

    fn notify_rate_limit(&self, _seconds_remaining: u64) {}

    fn clear_rate_limit(&self) {}
}
