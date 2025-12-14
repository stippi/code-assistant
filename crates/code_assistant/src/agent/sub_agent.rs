use crate::agent::persistence::AgentStatePersistence;
use crate::agent::{Agent, AgentComponents};
use crate::config::DefaultProjectManager;
use crate::permissions::PermissionMediator;
use crate::persistence::SessionModelConfig;
use crate::session::SessionConfig;
use crate::tools::core::ToolScope;
use crate::ui::{ToolStatus, UiEvent, UserInterface};
use anyhow::Result;
use command_executor::{CommandExecutor, DefaultCommandExecutor, SandboxedCommandExecutor};
use llm::Message;
use sandbox::{SandboxContext, SandboxPolicy};
use std::collections::HashMap;
use std::sync::{atomic::AtomicBool, atomic::Ordering, Arc, Mutex};
use std::time::SystemTime;

/// Cancellation registry keyed by the parent `spawn_agent` tool id.
#[derive(Default)]
pub struct SubAgentCancellationRegistry {
    flags: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl SubAgentCancellationRegistry {
    pub fn register(&self, tool_id: String) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        let mut flags = self.flags.lock().unwrap();
        flags.insert(tool_id, flag.clone());
        flag
    }

    pub fn cancel(&self, tool_id: &str) -> bool {
        let flags = self.flags.lock().unwrap();
        if let Some(flag) = flags.get(tool_id) {
            flag.store(true, Ordering::SeqCst);
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

/// Minimal in-memory persistence used for sub-agents.
struct NoOpStatePersistence;

impl AgentStatePersistence for NoOpStatePersistence {
    fn save_agent_state(&mut self, _state: crate::session::SessionState) -> Result<()> {
        Ok(())
    }
}

/// Runs sub-agents with isolated history and streams a compact progress view into the parent tool UI.
#[async_trait::async_trait]
/// Runs sub-agents with isolated history and streams a compact progress view into the parent tool UI.
#[async_trait::async_trait]
pub trait SubAgentRunner: Send + Sync {
    async fn run(
        &self,
        parent_tool_id: &str,
        instructions: String,
        tool_scope: ToolScope,
        require_file_references: bool,
    ) -> Result<String>;
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
}

impl DefaultSubAgentRunner {
    pub fn new(
        model_name: String,
        session_config: SessionConfig,
        sandbox_context: Arc<SandboxContext>,
        cancellation_registry: Arc<SubAgentCancellationRegistry>,
        ui: Arc<dyn UserInterface>,
        permission_handler: Option<Arc<dyn PermissionMediator>>,
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
        }
    }

    fn build_sub_agent_ui(
        &self,
        parent_ui: Arc<dyn UserInterface>,
        parent_tool_id: String,
        cancelled: Arc<AtomicBool>,
    ) -> Arc<SubAgentUiAdapter> {
        Arc::new(SubAgentUiAdapter::new(parent_ui, parent_tool_id, cancelled))
    }

    async fn build_agent(
        &self,
        parent_tool_id: &str,
        ui: Arc<dyn UserInterface>,
        cancelled: Arc<AtomicBool>,
        permission_handler: Option<Arc<dyn PermissionMediator>>,
    ) -> Result<Agent> {
        // Create a fresh LLM provider (avoid requiring Clone).
        let llm_provider =
            llm::factory::create_llm_client_from_model(&self.model_name, None, false, None).await?;

        // Create a fresh project manager, copying init_path if set.
        let mut project_manager: Box<dyn crate::config::ProjectManager> =
            Box::new(DefaultProjectManager::new());
        if let Some(path) = self.session_config.init_path.clone() {
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
            command_executor,
            ui,
            state_persistence: Box::new(NoOpStatePersistence),
            permission_handler,
            sub_agent_runner: None,
            sub_agent_cancellation_registry: Some(self.cancellation_registry.clone()),
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

        // Ensure sub-agent cancellation can interrupt streaming.
        agent.set_external_cancel_flag(cancelled);

        Ok(agent)
    }
}

fn tool_scope_for_subagent() -> ToolScope {
    // default; actual scope is set by caller via Agent::set_tool_scope
    ToolScope::SubAgentReadOnly
}

#[async_trait::async_trait]
impl SubAgentRunner for DefaultSubAgentRunner {
    async fn run(
        &self,
        parent_tool_id: &str,
        instructions: String,
        tool_scope: ToolScope,
        require_file_references: bool,
    ) -> Result<String> {
        let cancelled = self
            .cancellation_registry
            .register(parent_tool_id.to_string());
        let sub_ui = self.build_sub_agent_ui(
            self.ui.clone(),
            parent_tool_id.to_string(),
            cancelled.clone(),
        );

        // Keep a clone of the adapter so we can set the final response
        let sub_ui_adapter = sub_ui.clone();

        let mut agent = self
            .build_agent(
                parent_tool_id,
                sub_ui as Arc<dyn UserInterface>,
                cancelled.clone(),
                self.permission_handler.clone(),
            )
            .await?;
        agent.set_tool_scope(tool_scope);

        // Start with a single user message containing the full instructions.
        agent.append_message(Message::new_user(instructions))?;

        // Run 1+ iterations if we need to enforce file references.
        let mut last_answer = String::new();
        for attempt in 0..=2 {
            if cancelled.load(Ordering::SeqCst) {
                last_answer = "Sub-agent cancelled by user.".to_string();
                break;
            }

            agent.run_single_iteration().await?;

            last_answer = extract_last_assistant_text(agent.message_history()).unwrap_or_default();

            if !require_file_references {
                break;
            }

            if has_file_references_with_line_ranges(&last_answer) {
                break;
            }

            if attempt >= 2 {
                // Best-effort: return with warning.
                last_answer = format!(
                    "{last_answer}\n\n(Warning: requested file references with line ranges, but the sub-agent did not include them.)"
                );
                break;
            }

            // Ask the same sub-agent to revise.
            agent.append_message(Message::new_user(
                "Please revise your last answer to include exact file references with line ranges (e.g. `path/to/file.rs:10-20`).".to_string(),
            ))?;
        }

        self.cancellation_registry.unregister(parent_tool_id);

        // Set the final response in the adapter and send the complete JSON output
        // This preserves the tools list along with the final response for rendering
        sub_ui_adapter.set_response(last_answer.clone());
        let final_json = sub_ui_adapter.get_final_output();

        // Update the parent tool block with the complete output (tools + response)
        let _ = self
            .ui
            .send_event(UiEvent::UpdateToolStatus {
                tool_id: parent_tool_id.to_string(),
                status: ToolStatus::Success,
                message: Some("Sub-agent finished".to_string()),
                output: Some(final_json),
            })
            .await;

        Ok(last_answer)
    }
}

fn extract_last_assistant_text(messages: &[Message]) -> Option<String> {
    messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, llm::MessageRole::Assistant))
        .map(|m| m.to_string())
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
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
}

impl SubAgentOutput {
    pub fn new() -> Self {
        Self {
            tools: Vec::new(),
            activity: Some(SubAgentActivity::WaitingForLlm),
            cancelled: None,
            error: None,
            response: None,
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
}

impl SubAgentUiAdapter {
    fn new(
        parent: Arc<dyn UserInterface>,
        parent_tool_id: String,
        cancelled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            parent,
            parent_tool_id,
            cancelled,
            output: Mutex::new(SubAgentOutput::new()),
            tool_id_to_index: Mutex::new(std::collections::HashMap::new()),
        }
    }

    async fn send_output_update(&self) {
        let (json, tool_count, activity) = {
            let output = self.output.lock().unwrap();
            (output.to_json(), output.tools.len(), output.activity)
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
                status: ToolStatus::Running,
                message: Some("Sub-agent running".to_string()),
                output: Some(json),
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
            message: None,
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
                message,
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
    }

    fn set_activity(&self, activity: SubAgentActivity) {
        let mut output = self.output.lock().unwrap();
        output.activity = Some(activity);
    }

    fn set_response(&self, response: String) {
        let mut output = self.output.lock().unwrap();
        output.response = Some(response);
        output.activity = Some(SubAgentActivity::Completed);
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
            UiEvent::StartTool { name, id } => {
                // This event is typically not sent directly (GPUI creates it from DisplayFragment)
                // but handle it for completeness in case other UIs send it
                tracing::debug!(
                    "SubAgentUiAdapter: StartTool event received: {} ({})",
                    name,
                    id
                );
                self.add_tool_start(name, id);
                self.send_output_update().await;
            }

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

            UiEvent::StreamingStarted(_) => {
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
            DisplayFragment::ToolName { name, id } => {
                // A sub-agent tool is starting - capture it
                // Note: This is called during LLM streaming when the tool name is parsed
                // The UpdateToolStatus event with Running status comes later when execution starts
                tracing::debug!(
                    "SubAgentUiAdapter: ToolName fragment received: {} ({})",
                    name,
                    id
                );
                self.add_tool_start(name, id);
                // We can't send async update here, but the subsequent UpdateToolStatus
                // event will trigger send_output_update()
            }
            _ => {
                // Ignore other fragments (text, parameters, etc.)
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

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Helper for tool implementations to mark a tool id as cancelled.
pub fn cancel_sub_agent(registry: &SubAgentCancellationRegistry, tool_id: &str) -> bool {
    registry.cancel(tool_id)
}

/// Helper for tool implementations to create an initial visible tool result block.
pub fn sub_agent_tool_result_timestamps() -> (Option<SystemTime>, Option<SystemTime>) {
    (Some(SystemTime::now()), None)
}
