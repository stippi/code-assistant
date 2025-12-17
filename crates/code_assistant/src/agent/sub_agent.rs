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

/// Result from a sub-agent run, containing both the answer and UI output
#[derive(Debug, Clone)]
pub struct SubAgentResult {
    /// The plain text answer for LLM context
    pub answer: String,
    /// The JSON output for UI display (tools list + response)
    pub ui_output: String,
}

/// Runs sub-agents with isolated history and streams a compact progress view into the parent tool UI.
#[async_trait::async_trait]
pub trait SubAgentRunner: Send + Sync {
    async fn run(
        &self,
        parent_tool_id: &str,
        instructions: String,
        tool_scope: ToolScope,
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
    ) -> Result<SubAgentResult> {
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
                self.permission_handler.clone(),
            )
            .await?;
        agent.set_tool_scope(tool_scope);

        // Start with a single user message containing the full instructions.
        agent.append_message(Message::new_user(instructions))?;

        // Run 1+ iterations if we need to enforce file references.
        let mut last_answer = String::new();
        let mut was_cancelled = false;

        for attempt in 0..=2 {
            // Check for cancellation before starting iteration
            if cancelled.load(Ordering::SeqCst) {
                was_cancelled = true;
                break;
            }

            agent.run_single_iteration().await?;

            // Check for cancellation after iteration completes
            // (cancellation may have occurred during streaming/tool execution)
            if cancelled.load(Ordering::SeqCst) {
                was_cancelled = true;
                break;
            }

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

        // Handle cancellation: return error
        if was_cancelled {
            sub_ui_adapter.set_cancelled();
            return Err(anyhow::anyhow!("Cancelled by user"));
        }

        // Set the final response in the adapter and get the complete JSON output
        // This preserves the tools list along with the final response for rendering
        sub_ui_adapter.set_response(last_answer.clone());
        let final_json = sub_ui_adapter.get_final_output();

        Ok(SubAgentResult {
            answer: last_answer,
            ui_output: final_json,
        })
    }
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

        if let Some(&index) = id_map.get(tool_id) {
            if let Some(tool) = output.tools.get_mut(index) {
                // Append to existing parameter value (streaming may send chunks)
                let entry = tool.parameters.entry(name.to_string()).or_default();
                entry.push_str(value);

                // Update title from template using collected parameters
                if let Some(new_title) =
                    crate::tools::core::generate_tool_title(&tool.name, &tool.parameters)
                {
                    tool.title = Some(new_title);
                }
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
                // Ignore other fragments (text, etc.)
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
