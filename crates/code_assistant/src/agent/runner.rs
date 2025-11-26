use crate::agent::persistence::AgentStatePersistence;
use crate::agent::types::ToolExecution;
use crate::config::ProjectManager;
use crate::permissions::PermissionMediator;
use crate::persistence::{ChatMetadata, SessionModelConfig};
use crate::session::instance::SessionActivityState;
use crate::session::SessionConfig;
use crate::tools::core::{ResourcesTracker, ToolContext, ToolRegistry, ToolScope};
use crate::tools::{generate_system_message, ParserRegistry, ToolRequest};
use crate::types::*;
use crate::ui::{DisplayFragment, UiEvent, UserInterface};
use anyhow::Result;
use command_executor::CommandExecutor;
use llm::{
    ContentBlock, LLMProvider, LLMRequest, Message, MessageContent, MessageRole, StreamingCallback,
    StreamingChunk,
};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tracing::{debug, trace, warn};

/// Runtime components required to construct an `Agent`.
pub struct AgentComponents {
    pub llm_provider: Box<dyn LLMProvider>,
    pub project_manager: Box<dyn ProjectManager>,
    pub command_executor: Box<dyn CommandExecutor>,
    pub ui: Arc<dyn UserInterface>,
    pub state_persistence: Box<dyn AgentStatePersistence>,
    pub permission_handler: Option<Arc<dyn PermissionMediator>>,
}

use super::ToolSyntax;

/// Defines control flow for the agent loop.
enum LoopFlow {
    /// Continue to the next iteration of the loop.
    Continue,
    /// Break out of the loop, typically indicating task completion or critical error.
    Break,
    /// Get user input and then continue the loop.
    GetUserInput,
}

pub struct Agent {
    working_memory: WorkingMemory,
    plan: PlanState,
    llm_provider: Box<dyn LLMProvider>,
    session_config: SessionConfig,
    tool_scope: ToolScope,
    project_manager: Box<dyn ProjectManager>,
    command_executor: Box<dyn CommandExecutor>,
    ui: Arc<dyn UserInterface>,
    state_persistence: Box<dyn AgentStatePersistence>,
    permission_handler: Option<Arc<dyn PermissionMediator>>,
    // Store all messages exchanged
    message_history: Vec<Message>,
    // Store the history of tool executions
    tool_executions: Vec<crate::agent::types::ToolExecution>,
    // Cached system prompts keyed by model hint
    cached_system_prompts: HashMap<String, String>,
    // Optional model identifier used for prompt selection
    model_hint: Option<String>,
    // Model configuration associated with this session
    session_model_config: Option<SessionModelConfig>,
    // Optional override for the model's context window (primarily used in tests)
    context_limit_override: Option<u32>,
    // Counter for generating unique request IDs
    next_request_id: u64,
    // Session ID for this agent instance
    session_id: Option<String>,
    // The actual session name (empty if not named yet)
    session_name: String,
    // Whether to inject naming reminders (disabled for tests)
    enable_naming_reminders: bool,
    // Shared pending message with SessionInstance
    pending_message_ref: Option<Arc<Mutex<Option<String>>>>,
}

const CONTEXT_USAGE_THRESHOLD: f32 = 0.8;
const CONTEXT_COMPACTION_PROMPT: &str = include_str!("../../resources/compaction_prompt.md");

impl Agent {
    /// Formats an error, particularly ToolErrors, into a user-friendly string.
    fn format_error_for_user(error: &anyhow::Error) -> String {
        if let Some(tool_error) = error.downcast_ref::<ToolError>() {
            match tool_error {
                ToolError::UnknownTool(t) => {
                    format!("Unknown tool '{t}'. Please use only available tools.")
                }
                ToolError::ParseError(msg) => {
                    format!("Tool error: {msg}. Please try again.")
                }
            }
        } else {
            // Generic fallback for other error types
            format!("Error in tool request: {error}")
        }
    }

    pub fn new(components: AgentComponents, session_config: SessionConfig) -> Self {
        let AgentComponents {
            llm_provider,
            project_manager,
            command_executor,
            ui,
            state_persistence,
            permission_handler,
        } = components;

        let mut this = Self {
            working_memory: WorkingMemory::default(),
            plan: PlanState::default(),
            llm_provider,
            session_config,
            tool_scope: ToolScope::Agent, // Default to Agent scope
            project_manager,
            ui,
            command_executor,
            state_persistence,
            permission_handler,
            message_history: Vec::new(),
            tool_executions: Vec::new(),
            cached_system_prompts: HashMap::new(),
            session_model_config: None,
            context_limit_override: None,
            next_request_id: 1, // Start from 1
            session_id: None,
            session_name: String::new(),
            enable_naming_reminders: true, // Enabled by default
            pending_message_ref: None,
            model_hint: None,
        };
        if this.session_config.use_diff_blocks {
            this.tool_scope = ToolScope::AgentWithDiffBlocks;
        }
        this
    }

    /// Enable diff blocks format for file editing (uses replace_in_file tool instead of edit)
    pub fn enable_diff_blocks(&mut self) {
        self.tool_scope = ToolScope::AgentWithDiffBlocks;
        self.session_config.use_diff_blocks = true;
        // Clear cached system message so it gets regenerated with the new scope
        self.invalidate_system_message_cache();
    }

    fn tool_syntax(&self) -> ToolSyntax {
        self.session_config.tool_syntax
    }

    /// Set the shared pending message reference from SessionInstance
    pub fn set_pending_message_ref(&mut self, pending_ref: Arc<Mutex<Option<String>>>) {
        self.pending_message_ref = Some(pending_ref);
    }

    /// Update the model hint used for selecting system prompts
    pub fn set_model_hint(&mut self, model_hint: Option<String>) {
        let normalized = model_hint.and_then(|hint| {
            let trimmed = hint.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });

        if self.model_hint != normalized {
            self.model_hint = normalized;
            self.invalidate_system_message_cache();
        }
    }

    /// Disable naming reminders (used for tests)
    #[cfg(test)]
    pub fn disable_naming_reminders(&mut self) {
        self.enable_naming_reminders = false;
    }

    /// Set session name (used for tests)
    #[cfg(test)]
    pub(crate) fn set_session_name(&mut self, name: String) {
        self.session_name = name;
    }

    /// Get and clear the pending message from shared state
    fn get_and_clear_pending_message(&self) -> Option<String> {
        if let Some(ref pending_ref) = self.pending_message_ref {
            let mut pending = pending_ref.lock().unwrap();
            pending.take()
        } else {
            None
        }
    }

    /// Check if there is a pending message (without clearing it)
    fn has_pending_message(&self) -> bool {
        if let Some(ref pending_ref) = self.pending_message_ref {
            let pending = pending_ref.lock().unwrap();
            pending.is_some()
        } else {
            false
        }
    }

    async fn update_activity_state(&self, new_state: SessionActivityState) -> Result<()> {
        if let Some(session_id) = &self.session_id {
            self.ui
                .send_event(UiEvent::UpdateSessionActivityState {
                    session_id: session_id.clone(),
                    activity_state: new_state,
                })
                .await?;
        }
        Ok(())
    }

    /// Build current session metadata
    fn build_current_metadata(&self) -> Option<ChatMetadata> {
        // Only build metadata if we have a session ID
        self.session_id.as_ref().map(|session_id| {
            // Calculate total usage and find last usage across all messages
            let mut total_usage = llm::Usage::zero();
            let mut last_usage = llm::Usage::zero();

            for message in &self.message_history {
                if let Some(usage) = &message.usage {
                    total_usage.input_tokens += usage.input_tokens;
                    total_usage.output_tokens += usage.output_tokens;
                    total_usage.cache_creation_input_tokens += usage.cache_creation_input_tokens;
                    total_usage.cache_read_input_tokens += usage.cache_read_input_tokens;

                    // For assistant messages, update last usage (most recent wins)
                    if matches!(message.role, MessageRole::Assistant) {
                        last_usage = usage.clone();
                    }
                }
            }

            // Use default for tokens limit - will be updated by persistence layer
            let tokens_limit = None;

            ChatMetadata {
                id: session_id.clone(),
                name: self.session_name.clone(), // Empty string if not named yet
                created_at: SystemTime::now(),   // Will be overridden by persistence
                updated_at: SystemTime::now(),
                message_count: self.message_history.len(),
                total_usage,
                last_usage,
                tokens_limit,
                tool_syntax: self.tool_syntax(),
                initial_project: if self.session_config.initial_project.is_empty() {
                    "unknown".to_string()
                } else {
                    self.session_config.initial_project.clone()
                },
            }
        })
    }

    /// Save the current state (message history and tool executions)
    fn save_state(&mut self) -> Result<()> {
        trace!(
            "saving {} messages to persistence",
            self.message_history.len()
        );
        if let Some(session_id) = &self.session_id {
            let session_state = crate::session::SessionState {
                session_id: session_id.clone(),
                name: self.session_name.clone(),
                messages: self.message_history.clone(),
                tool_executions: self.tool_executions.clone(),
                working_memory: self.working_memory.clone(),
                plan: self.plan.clone(),
                config: self.session_config.clone(),
                next_request_id: Some(self.next_request_id),
                model_config: self.session_model_config.clone(),
            };
            self.state_persistence.save_agent_state(session_state)?;
        }

        // Send updated session metadata to UI
        if let Some(metadata) = self.build_current_metadata() {
            let _ = tokio::runtime::Handle::try_current().map(|_| {
                let ui = self.ui.clone();
                let metadata = metadata.clone();
                tokio::spawn(async move {
                    let _ = ui
                        .send_event(UiEvent::UpdateSessionMetadata { metadata })
                        .await;
                });
            });
        }

        Ok(())
    }

    /// Adds a message to the history and saves the state
    pub fn append_message(&mut self, message: Message) -> Result<()> {
        self.message_history.push(message);
        self.save_state()?;
        Ok(())
    }

    /// Run a single iteration of the agent loop without waiting for user input
    /// This is used in the new on-demand agent architecture
    pub async fn run_single_iteration(&mut self) -> Result<()> {
        loop {
            // Check for pending user message and add it to history at start of each iteration
            if let Some(pending_message) = self.get_and_clear_pending_message() {
                debug!("Processing pending user message: {}", pending_message);
                self.append_message(Message::new_user(pending_message.clone()))?;

                // Notify UI about the user message
                self.ui
                    .send_event(UiEvent::DisplayUserInput {
                        content: pending_message,
                        attachments: Vec::new(),
                    })
                    .await?;
            }

            if self.should_trigger_compaction()? {
                self.perform_compaction().await?;
                continue;
            }

            let messages = self.render_tool_results_in_messages();

            // 1. Get LLM response (without adding to history yet)
            let (llm_response, request_id) = self.get_next_assistant_message(messages).await?;

            // 2. Add original LLM response to message history if it has content
            if !llm_response.content.is_empty() {
                self.append_message(
                    Message::new_assistant_content(llm_response.content.clone())
                        .with_request_id(request_id)
                        .with_usage(llm_response.usage.clone()),
                )?;
            }

            // 3. Extract tool requests from LLM response and get truncated response
            let (tool_requests, flow, truncated_response) = self
                .extract_tool_requests_from_response(&llm_response, request_id)
                .await?;

            // 4. If we have a truncated response different from the original, update the last message
            if !truncated_response.content.is_empty()
                && !self.message_history.is_empty()
                && truncated_response.content != llm_response.content
            {
                // Replace the last message with the truncated version
                if let Some(last_msg) = self.message_history.last_mut() {
                    if last_msg.role == MessageRole::Assistant {
                        last_msg.content =
                            MessageContent::Structured(truncated_response.content.clone());
                        last_msg.usage = Some(truncated_response.usage.clone());
                    }
                }
            }

            // 5. Act based on the flow instruction
            match flow {
                LoopFlow::GetUserInput => {
                    // In on-demand mode, we don't wait for user input
                    // Instead, we complete this iteration
                    debug!("Agent iteration complete - waiting for next user message");
                    return Ok(());
                }
                LoopFlow::Continue => {
                    if !tool_requests.is_empty() {
                        // Tools were requested, manage their execution
                        let flow = self.manage_tool_execution(&tool_requests).await?;

                        // Save state and update memory after tool executions
                        self.save_state()?;
                        let _ = self
                            .ui
                            .send_event(UiEvent::UpdateMemory {
                                memory: self.working_memory.clone(),
                            })
                            .await;

                        match flow {
                            LoopFlow::Continue => { /* Continue to the next iteration */ }
                            LoopFlow::GetUserInput => {
                                // Complete iteration instead of waiting for input
                                debug!("Tool execution complete - waiting for next user message");
                                return Ok(());
                            }
                            LoopFlow::Break => {
                                // Task completed (e.g., via complete_task tool)
                                debug!("Task completed");
                                return Ok(());
                            }
                        }
                    }
                    // If tool_requests is empty with Continue flow, this means there was a parse error
                    // and we should continue the loop to give the LLM another chance to respond correctly
                }
                LoopFlow::Break => {
                    // Loop completed
                    debug!("Agent loop break requested");
                    // Save state before returning
                    self.save_state()?;
                    return Ok(());
                }
            }
        }
    }

    /// Load state from session state (for backward compatibility)
    pub async fn load_from_session_state(
        &mut self,
        session_state: crate::session::SessionState,
    ) -> Result<()> {
        // Restore all state components
        self.session_id = Some(session_state.session_id);
        self.message_history = session_state.messages;
        debug!(
            "loaded {} messages from session",
            self.message_history.len()
        );
        self.tool_executions = session_state.tool_executions;
        self.working_memory = session_state.working_memory;
        self.plan = session_state.plan.clone();
        self.session_config = session_state.config;
        if self.session_config.use_diff_blocks {
            self.enable_diff_blocks();
        } else {
            self.session_config.use_diff_blocks = false;
            self.tool_scope = ToolScope::Agent;
            self.invalidate_system_message_cache();
        }
        self.normalize_loaded_message_history();
        self.session_name = session_state.name;
        self.session_model_config = session_state.model_config;
        self.context_limit_override = None;
        if let Some(model_config) = self.session_model_config.as_mut() {
            let model_name = model_config.model_name.clone();
            self.set_model_hint(Some(model_name));
        }

        // Restore next_request_id from session, or calculate from existing messages for backward compatibility
        self.next_request_id = session_state.next_request_id.unwrap_or_else(|| {
            self.message_history
                .iter()
                .filter(|msg| matches!(msg.role, llm::MessageRole::Assistant))
                .count() as u64
                + 1
        });

        // Restore working memory file trees and project state
        self.init_working_memory_projects()?;

        let _ = self
            .ui
            .send_event(UiEvent::UpdateMemory {
                memory: self.working_memory.clone(),
            })
            .await;

        let _ = self
            .ui
            .send_event(UiEvent::UpdatePlan {
                plan: self.plan.clone(),
            })
            .await;

        Ok(())
    }

    fn normalize_loaded_message_history(&mut self) {
        if self.message_history.is_empty() {
            return;
        }

        let parser = ParserRegistry::get(self.tool_syntax());
        let parser = parser.as_ref();
        let mut removed = 0usize;

        loop {
            let Some(last_assistant_idx) = self
                .message_history
                .iter()
                .rposition(|message| message.role == MessageRole::Assistant)
            else {
                break;
            };

            let last_assistant = &self.message_history[last_assistant_idx];

            if !parser.message_contains_tool_invocation(last_assistant) {
                break;
            }

            let has_tool_result_after = self.message_history[last_assistant_idx + 1..]
                .iter()
                .any(Self::is_user_tool_result_message);

            if has_tool_result_after {
                break;
            }

            let message = self.message_history.remove(last_assistant_idx);
            debug!(
                "Removing dangling assistant tool request (request_id={:?}) from history",
                message.request_id
            );
            removed += 1;
        }

        if removed > 0 {
            debug!(
                "Normalized message history by dropping {removed} dangling tool request message(s)"
            );
        }
    }

    fn is_user_tool_result_message(message: &Message) -> bool {
        if message.role != MessageRole::User {
            return false;
        }

        match &message.content {
            MessageContent::Structured(blocks) => blocks
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolResult { .. })),
            MessageContent::Text(text) => text.trim().is_empty(),
        }
    }

    #[allow(dead_code)]
    pub fn init_working_memory(&mut self) -> Result<()> {
        // Initialize empty structures for multi-project support
        self.working_memory.file_trees = HashMap::new();
        self.working_memory.available_projects = Vec::new();

        // Reset the initial project
        self.session_config.initial_project = String::new();

        self.init_working_memory_projects()
    }

    fn init_working_memory_projects(&mut self) -> Result<()> {
        // If a path was provided in args, add it as a temporary project
        if let Some(path) = &self.session_config.init_path {
            // Add as temporary project and get its name
            let project_name = self.project_manager.add_temporary_project(path.clone())?;

            // Store the name of the initial project
            self.session_config.initial_project = project_name.clone();

            // Create initial file tree for this project
            let mut explorer = self
                .project_manager
                .get_explorer_for_project(&project_name)?;
            let tree = explorer.create_initial_tree(2)?; // Limited depth for initial tree

            // Store in working memory
            self.working_memory
                .file_trees
                .insert(project_name.clone(), tree);
        }

        // Load all available projects
        let all_projects = self.project_manager.get_projects()?;
        for project_name in all_projects.keys() {
            if !self
                .working_memory
                .available_projects
                .contains(project_name)
            {
                self.working_memory
                    .available_projects
                    .push(project_name.clone());
            }
        }

        Ok(())
    }

    /// Parses tool requests from the LLM response and returns a truncated response.
    /// Returns a tuple of tool requests, LoopFlow, and truncated LLM response.
    /// - If parsing succeeds and requests are empty: returns (empty vec, GetUserInput, truncated_response)
    /// - If parsing succeeds and requests exist: returns (requests, Continue, truncated_response)
    /// - If parsing fails: adds an error message to history and returns (empty vec, Continue, original_response)
    async fn extract_tool_requests_from_response(
        &mut self,
        llm_response: &llm::LLMResponse,
        request_counter: u64,
    ) -> Result<(Vec<ToolRequest>, LoopFlow, llm::LLMResponse)> {
        let parser = ParserRegistry::get(self.tool_syntax());
        match parser.extract_requests(llm_response, request_counter, 0) {
            Ok((requests, truncated_response)) => {
                if requests.is_empty() && !self.has_pending_message() {
                    Ok((requests, LoopFlow::GetUserInput, truncated_response))
                } else {
                    Ok((requests, LoopFlow::Continue, truncated_response))
                }
            }
            Err(e) => {
                let error_text = Self::format_error_for_user(&e);

                let error_msg = match self.tool_syntax() {
                    ToolSyntax::Native => {
                        // For native mode, keep text message since parsing errors occur before
                        // we have any LLM-provided tool IDs to reference
                        Message::new_user(error_text)
                    }
                    _ => {
                        // For custom tool-syntax modes, create structured tool-result message like regular tool results
                        // Generate normal tool ID for consistency with UI expectations
                        let tool_id = format!("tool-{request_counter}-1");

                        // Create and store a ToolExecution for the parse error
                        let tool_execution =
                            ToolExecution::create_parse_error(tool_id.clone(), error_text.clone());
                        self.tool_executions.push(tool_execution);

                        Message::new_user_content(vec![ContentBlock::ToolResult {
                            tool_use_id: tool_id,
                            content: error_text,
                            is_error: Some(true),
                            start_time: Some(SystemTime::now()),
                            end_time: None,
                        }])
                    }
                };

                self.append_message(error_msg)?;
                // Return original response for error cases
                Ok((Vec::new(), LoopFlow::Continue, llm_response.clone())) // Continue without user input on parsing errors
            }
        }
    }

    /// Executes a list of tool requests.
    /// Handles the "complete_task" action and appends tool results to message history.
    async fn manage_tool_execution(&mut self, tool_requests: &[ToolRequest]) -> Result<LoopFlow> {
        let mut content_blocks = Vec::new();

        for tool_request in tool_requests {
            if tool_request.name == "complete_task" {
                debug!("Task completed");
                return Ok(LoopFlow::Break);
            }

            let start_time = Some(SystemTime::now());
            let result_block = match self.execute_tool(tool_request).await {
                Ok(success) => ContentBlock::ToolResult {
                    tool_use_id: tool_request.id.clone(),
                    content: String::new(), // Will be filled dynamically in prepare_messages
                    is_error: if success { None } else { Some(true) },
                    start_time,
                    end_time: Some(SystemTime::now()),
                },
                Err(e) => {
                    let error_text = Self::format_error_for_user(&e);
                    ContentBlock::ToolResult {
                        tool_use_id: tool_request.id.clone(),
                        content: error_text,
                        is_error: Some(true),
                        start_time,
                        end_time: Some(SystemTime::now()),
                    }
                }
            };
            content_blocks.push(result_block);
        }

        // Only add message if there were actual tool executions (not just complete_task)
        if !content_blocks.is_empty() {
            let result_message = Message::new_user_content(content_blocks);
            self.append_message(result_message)?;
        }
        Ok(LoopFlow::Continue)
    }

    /// Start a new agent task
    #[cfg(test)]
    pub async fn start_with_task(&mut self, task: String) -> Result<()> {
        debug!("Starting agent with task: {}", task);

        self.init_working_memory()?;

        self.message_history.clear();
        self.ui
            .send_event(UiEvent::DisplayUserInput {
                content: task.clone(),
                attachments: Vec::new(),
            })
            .await?;

        // Create the initial user message
        self.append_message(Message::new_user(task.clone()))?;

        // Notify UI of initial working memory
        let _ = self
            .ui
            .send_event(UiEvent::UpdateMemory {
                memory: self.working_memory.clone(),
            })
            .await;

        self.run_single_iteration().await
    }

    /// Get the appropriate system prompt based on tool mode
    fn get_system_prompt(&mut self) -> String {
        let cache_key = self
            .model_hint
            .as_deref()
            .map(|hint| hint.to_ascii_lowercase())
            .unwrap_or_default();

        if let Some(cached) = self.cached_system_prompts.get(&cache_key) {
            return cached.clone();
        }

        // Generate the system message using the tools module
        let mut system_message = generate_system_message(
            self.tool_syntax(),
            self.tool_scope,
            self.model_hint.as_deref(),
        );

        // Add project information
        let mut project_info = String::new();

        // Add information about the initial project if available
        if !self.session_config.initial_project.is_empty() {
            project_info.push_str("\n\n# Project Information\n\n");
            project_info.push_str(&format!(
                "## Initial Project: {}\n\n",
                self.session_config.initial_project
            ));

            // Add file tree for the initial project if available
            if let Some(tree) = self
                .working_memory
                .file_trees
                .get(&self.session_config.initial_project)
            {
                project_info.push_str("### File Structure:\n");
                project_info.push_str(&format!("```\n{tree}\n```\n\n"));
            }
        }

        // Add information about available projects
        if !self.working_memory.available_projects.is_empty() {
            project_info.push_str("## Available Projects:\n");
            for project in &self.working_memory.available_projects {
                project_info.push_str(&format!("- {project}\n"));
            }
        }

        // Append project information to base prompt if available
        if !project_info.is_empty() {
            system_message = format!("{system_message}\n{project_info}");
        }

        // Append repository guidance file if present (AGENTS.md preferred, else CLAUDE.md)
        let guidance = self.read_repository_guidance();
        if let Some((file_name, content)) = guidance {
            let mut guidance_section = String::new();
            guidance_section.push_str("\n\n# Repository Guidance\n\n");
            guidance_section.push_str(&format!("Loaded from `{file_name}`.\n\n"));
            guidance_section.push_str(&content);
            system_message.push_str(&guidance_section);
        }

        // Cache the system message for the current model hint
        self.cached_system_prompts
            .insert(cache_key, system_message.clone());

        system_message
    }

    /// Attempt to read AGENTS.md or CLAUDE.md from the initial project root.
    /// Prefers AGENTS.md when both exist. Returns (file_name, content) on success.
    fn read_repository_guidance(&self) -> Option<(String, String)> {
        // Determine search root
        let root_path = if !self.session_config.initial_project.is_empty() {
            PathBuf::from(&self.session_config.initial_project)
        } else {
            std::env::current_dir().ok()?
        };

        // Candidate files in priority order
        let candidates = ["AGENTS.md", "CLAUDE.md"];

        for file in candidates.iter() {
            let path = root_path.join(file);
            if path.exists() {
                match fs::read_to_string(&path) {
                    Ok(mut content) => {
                        // Guard against excessively large files (truncate politely)
                        const MAX_LEN: usize = 64 * 1024; // 64KB
                        if content.len() > MAX_LEN {
                            content.truncate(MAX_LEN);
                            content.push_str(
                                "\n\n[... truncated to keep context size reasonable ...]",
                            );
                        }
                        return Some((file.to_string(), content));
                    }
                    Err(e) => {
                        warn!("Failed to read guidance file {}: {}", path.display(), e);
                    }
                }
            }
        }

        None
    }

    /// Invalidate the cached system message to force regeneration
    pub fn invalidate_system_message_cache(&mut self) {
        self.cached_system_prompts.clear();
    }

    /// Convert ToolResult blocks to Text blocks for custom tool-syntax mode
    fn convert_tool_results_to_text(&self, messages: Vec<Message>) -> Vec<Message> {
        // Create a fresh ResourcesTracker for rendering
        let mut resources_tracker = crate::tools::core::render::ResourcesTracker::new();

        // First, build a map of tool_use_id to rendered output
        let mut tool_outputs = std::collections::HashMap::new();

        // Process tool executions in reverse chronological order (newest first)
        for execution in self.tool_executions.iter().rev() {
            let tool_use_id = &execution.tool_request.id;
            let rendered_output = execution.result.as_render().render(&mut resources_tracker);
            tool_outputs.insert(tool_use_id.clone(), rendered_output);
        }

        // Process each message
        messages
            .into_iter()
            .map(|msg| {
                match &msg.content {
                    MessageContent::Structured(blocks) => {
                        // Check if there are any ToolResult blocks that need conversion
                        let has_tool_results = blocks
                            .iter()
                            .any(|block| matches!(block, ContentBlock::ToolResult { .. }));

                        if !has_tool_results {
                            // No conversion needed
                            return msg;
                        }

                        // Convert all blocks to Text
                        let mut text_content = String::new();

                        for block in blocks {
                            match block {
                                ContentBlock::ToolResult { tool_use_id, .. } => {
                                    // Get the dynamically rendered content for this tool result
                                    if let Some(rendered_output) = tool_outputs.get(tool_use_id) {
                                        // Add the rendered tool output from actual tool execution
                                        text_content.push_str(rendered_output);
                                        text_content.push_str("\n\n");
                                    }
                                }
                                ContentBlock::Text { text, .. } => {
                                    // For existing Text blocks, keep as is
                                    text_content.push_str(text);
                                    text_content.push_str("\n\n");
                                }
                                _ => {} // Ignore other block types
                            }
                        }

                        // Create a new message with Text content
                        Message {
                            role: msg.role,
                            content: MessageContent::Text(text_content.trim().to_string()),
                            request_id: msg.request_id,
                            usage: msg.usage.clone(),
                            ..Default::default()
                        }
                    }
                    // For non-structured content, keep as is
                    _ => msg,
                }
            })
            .collect()
    }

    /// Inject system reminder for session naming if needed
    pub(crate) fn inject_naming_reminder_if_needed(
        &self,
        mut messages: Vec<Message>,
    ) -> Vec<Message> {
        // Only inject if enabled, session is not named yet, and we have messages
        if !self.enable_naming_reminders || !self.session_name.is_empty() || messages.is_empty() {
            return messages;
        }

        // Find the last actual user message (not tool results) and add system reminder
        // Iterate backwards through messages to find the last user message with actual content
        for msg in messages.iter_mut().rev() {
            if matches!(msg.role, MessageRole::User) {
                let is_actual_user_message = match &msg.content {
                    MessageContent::Text(_) => true, // Text content is always actual user input
                    MessageContent::Structured(blocks) => {
                        // Check if this message contains tool results
                        // If it contains only ToolResult blocks, it's not an actual user message
                        blocks
                            .iter()
                            .any(|block| !matches!(block, ContentBlock::ToolResult { .. }))
                    }
                };

                if is_actual_user_message {
                    let reminder_text = "<system-reminder>\nThis is an automatic reminder from the system. Please use the `name_session` tool first, provided the user has already given you a clear task or question. You can chain additional tools after using the `name_session` tool.\n</system-reminder>";

                    trace!("Injecting session naming reminder to actual user message");

                    match &mut msg.content {
                        MessageContent::Text(original_text) => {
                            // Convert from Text to Structured with two ContentBlocks
                            msg.content = MessageContent::Structured(vec![
                                ContentBlock::new_text(original_text.clone()),
                                ContentBlock::new_text(reminder_text.to_string()),
                            ]);
                        }
                        MessageContent::Structured(blocks) => {
                            // Add reminder as a new ContentBlock
                            blocks.push(ContentBlock::new_text(reminder_text));
                        }
                    }
                    break; // Found and updated the last actual user message, we're done
                }
            }
        }

        messages
    }

    /// Gets the next assistant message from the LLM provider.
    async fn get_next_assistant_message(
        &mut self,
        messages: Vec<Message>,
    ) -> Result<(llm::LLMResponse, u64)> {
        // Generate and increment request ID
        let request_id = self.next_request_id;
        self.next_request_id += 1;

        // Inform UI that a new LLM request is starting
        self.ui
            .send_event(UiEvent::StreamingStarted(request_id))
            .await?;
        debug!("Starting LLM request with ID: {}", request_id);

        // Inject naming reminder if session is not named yet
        let messages_with_reminder = self.inject_naming_reminder_if_needed(messages);

        // Convert messages based on tool syntax
        // Native mode keeps ToolUse blocks, all other syntaxes convert to text
        let converted_messages = match self.tool_syntax() {
            ToolSyntax::Native => messages_with_reminder, // No conversion needed
            _ => self.convert_tool_results_to_text(messages_with_reminder), // Convert ToolResults to Text
        };

        let request = LLMRequest {
            messages: converted_messages,
            system_prompt: self.get_system_prompt(),
            tools: match self.tool_syntax() {
                ToolSyntax::Native => {
                    Some(crate::tools::AnnotatedToolDefinition::to_tool_definitions(
                        ToolRegistry::global().get_tool_definitions_for_scope(self.tool_scope),
                    ))
                }
                ToolSyntax::Xml => None,
                ToolSyntax::Caret => None,
            },
            stop_sequences: None,
            request_id,
            session_id: self.session_id.clone().unwrap_or_default(),
        };

        // Log messages for debugging
        /*
        for (i, message) in request.messages.iter().enumerate() {
            debug!("Message {}:", i);
            debug!("Message {}:", i);
            // Using the Display trait implementation for Message
            let formatted_message = format!("{message}");
            // Add indentation to the message output
            let indented = formatted_message
                .lines()
                .map(|line| format!("  {line}"))
                .collect::<Vec<String>>()
                .join("\n");
            debug!("{}", indented);
        }
        */

        // Create a StreamProcessor with the UI and request ID
        let parser = ParserRegistry::get(self.tool_syntax());
        let processor = Arc::new(Mutex::new(
            parser.stream_processor(self.ui.clone(), request_id),
        ));

        let ui_for_callback = self.ui.clone();
        let streaming_callback: StreamingCallback = Box::new(move |chunk: &StreamingChunk| {
            // Check if streaming should continue
            if !ui_for_callback.should_streaming_continue() {
                debug!("Streaming should stop - user requested cancellation");
                return Err(anyhow::anyhow!("Streaming cancelled by user"));
            }

            let mut processor_guard = processor.lock().unwrap();
            processor_guard
                .process(chunk)
                .map_err(|e| anyhow::anyhow!("Failed to process streaming chunk: {e}"))
        });

        // Send message to LLM provider
        let response = match self
            .llm_provider
            .send_message(request, Some(&streaming_callback))
            .await
        {
            Ok(response) => response,
            Err(e) => {
                // Check for streaming cancelled error
                if e.to_string().contains("Streaming cancelled by user") {
                    debug!("Streaming cancelled by user in LLM request {}", request_id);
                    // End LLM request with cancelled=true
                    let _ = self
                        .ui
                        .send_event(UiEvent::StreamingStopped {
                            id: request_id,
                            cancelled: true,
                            error: None,
                        })
                        .await;
                    // Return empty response
                    return Ok((
                        llm::LLMResponse {
                            content: Vec::new(),
                            usage: llm::Usage::zero(),
                            rate_limit_info: None,
                        },
                        request_id,
                    ));
                }

                // For other errors, still end the request but not cancelled
                let _ = self
                    .ui
                    .send_event(UiEvent::StreamingStopped {
                        id: request_id,
                        cancelled: false,
                        error: Some(e.to_string()),
                    })
                    .await;
                return Err(e);
            }
        };

        // Print response for debugging
        debug!("Raw LLM response:");
        for block in &response.content {
            match block {
                ContentBlock::Text { text, .. } => {
                    debug!("---\n{}\n---", text);
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    debug!("---\ntool: {}, input: {}\n---", name, input);
                }
                _ => {}
            }
        }

        debug!(
            "Token usage: Input: {}, Output: {}, Cache: Created: {}, Read: {}",
            response.usage.input_tokens,
            response.usage.output_tokens,
            response.usage.cache_creation_input_tokens,
            response.usage.cache_read_input_tokens
        );

        // Inform UI that the LLM request has completed (normal completion)
        let _ = self
            .ui
            .send_event(UiEvent::StreamingStopped {
                id: request_id,
                cancelled: false,
                error: None,
            })
            .await;
        debug!("Completed LLM request with ID: {}", request_id);

        Ok((response, request_id))
    }

    async fn get_non_streaming_response(
        &mut self,
        messages: Vec<Message>,
    ) -> Result<(llm::LLMResponse, u64)> {
        let request_id = self.next_request_id;
        self.next_request_id += 1;

        let messages_with_reminder = self.inject_naming_reminder_if_needed(messages);

        let converted_messages = match self.tool_syntax() {
            ToolSyntax::Native => messages_with_reminder,
            _ => self.convert_tool_results_to_text(messages_with_reminder),
        };

        let request = LLMRequest {
            messages: converted_messages,
            system_prompt: self.get_system_prompt(),
            tools: match self.tool_syntax() {
                ToolSyntax::Native => {
                    Some(crate::tools::AnnotatedToolDefinition::to_tool_definitions(
                        ToolRegistry::global().get_tool_definitions_for_scope(self.tool_scope),
                    ))
                }
                ToolSyntax::Xml => None,
                ToolSyntax::Caret => None,
            },
            stop_sequences: None,
            request_id,
            session_id: self.session_id.clone().unwrap_or_default(),
        };

        let response = self.llm_provider.send_message(request, None).await?;

        debug!(
            "Compaction response usage — Input: {}, Output: {}, Cache Read: {}",
            response.usage.input_tokens,
            response.usage.output_tokens,
            response.usage.cache_read_input_tokens
        );

        Ok((response, request_id))
    }

    fn format_compaction_summary_for_prompt(summary: &str) -> String {
        let trimmed = summary.trim();
        if trimmed.is_empty() {
            "Conversation summary: (empty)".to_string()
        } else {
            format!("Conversation summary:\n{trimmed}")
        }
    }

    fn extract_compaction_summary_text(blocks: &[ContentBlock]) -> String {
        let mut collected = Vec::new();
        for block in blocks {
            match block {
                ContentBlock::Text { text, .. } => collected.push(text.as_str()),
                ContentBlock::Thinking { thinking, .. } => {
                    collected.push(thinking.as_str());
                }
                _ => {}
            }
        }

        let merged = collected.join("\n").trim().to_string();
        if merged.is_empty() {
            "No summary was generated.".to_string()
        } else {
            merged
        }
    }

    fn active_messages(&self) -> &[Message] {
        if self.message_history.is_empty() {
            return &[];
        }
        let start = self
            .message_history
            .iter()
            .rposition(|message| message.is_compaction_summary)
            .unwrap_or(0);
        &self.message_history[start..]
    }

    #[cfg(test)]
    pub fn set_test_session_metadata(
        &mut self,
        session_id: String,
        model_config: SessionModelConfig,
    ) {
        self.session_id = Some(session_id);
        self.session_model_config = Some(model_config);
    }

    #[cfg(test)]
    pub fn set_test_context_limit(&mut self, limit: u32) {
        self.context_limit_override = Some(limit);
    }

    #[cfg(test)]
    pub fn message_history_for_tests(&self) -> &Vec<Message> {
        &self.message_history
    }

    fn context_usage_ratio(&mut self) -> Result<Option<f32>> {
        let model_name = match self.session_model_config.as_ref() {
            Some(config) => config.model_name.clone(),
            None => return Ok(None),
        };

        let limit = if let Some(limit) = self.context_limit_override {
            limit
        } else {
            let config_system = llm::provider_config::ConfigurationSystem::load()?;
            config_system
                .get_model(&model_name)
                .map(|model| model.context_token_limit)
                .ok_or_else(|| anyhow::anyhow!("Model not found in models.json: {model_name}"))?
        };

        if limit == 0 {
            return Ok(None);
        }

        for message in self.active_messages().iter().rev() {
            if !matches!(message.role, MessageRole::Assistant) {
                continue;
            }
            if let Some(usage) = &message.usage {
                let used_tokens = usage.input_tokens + usage.cache_read_input_tokens;
                if used_tokens > 0 {
                    return Ok(Some(used_tokens as f32 / limit as f32));
                }
            }
        }

        Ok(None)
    }

    fn should_trigger_compaction(&mut self) -> Result<bool> {
        if let Some(ratio) = self.context_usage_ratio()? {
            if ratio >= CONTEXT_USAGE_THRESHOLD {
                debug!(
                    "Context usage {:.1}% >= threshold {:.0}% — triggering compaction",
                    ratio * 100.0,
                    CONTEXT_USAGE_THRESHOLD * 100.0
                );
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn perform_compaction(&mut self) -> Result<()> {
        debug!("Starting context compaction");

        let compaction_message = Message {
            role: MessageRole::User,
            content: MessageContent::Text(CONTEXT_COMPACTION_PROMPT.to_string()),
            ..Default::default()
        };

        let mut messages = self.render_tool_results_in_messages();
        messages.push(compaction_message);
        self.update_activity_state(SessionActivityState::WaitingForResponse)
            .await?;
        let response_result = self.get_non_streaming_response(messages).await;
        self.update_activity_state(SessionActivityState::AgentRunning)
            .await?;
        let (response, _) = response_result?;

        let summary_text = Self::extract_compaction_summary_text(&response.content);
        let summary_message = Message {
            role: MessageRole::User,
            content: MessageContent::Text(summary_text.clone()),
            is_compaction_summary: true,
            ..Default::default()
        };
        self.append_message(summary_message)?;

        let divider = DisplayFragment::CompactionDivider {
            summary: summary_text.trim().to_string(),
        };
        self.ui.display_fragment(&divider)?;

        Ok(())
    }

    /// Prepare messages for LLM request, dynamically rendering tool outputs
    fn render_tool_results_in_messages(&self) -> Vec<Message> {
        // Start with a clean slate
        let mut messages = Vec::new();

        // Create a fresh ResourcesTracker for this rendering pass
        let mut resources_tracker = crate::tools::core::render::ResourcesTracker::new();

        // First, collect all tool executions and build a map from tool_use_id to rendered output
        let mut tool_outputs = std::collections::HashMap::new();

        // Process tool executions in reverse chronological order (newest first)
        // so newer tool calls take precedence in resource conflicts
        for execution in self.tool_executions.iter().rev() {
            let tool_use_id = &execution.tool_request.id;
            let rendered_output = execution.result.as_render().render(&mut resources_tracker);
            tool_outputs.insert(tool_use_id.clone(), rendered_output);
        }

        // Now rebuild the message history, replacing tool outputs with our dynamically rendered versions
        for msg in self.active_messages() {
            match &msg.content {
                MessageContent::Structured(blocks) => {
                    // Look for ToolResult blocks
                    let mut new_blocks = Vec::new();
                    let mut need_update = false;

                    for block in blocks {
                        match block {
                            ContentBlock::ToolResult {
                                tool_use_id,
                                is_error,
                                start_time,
                                end_time,
                                ..
                            } => {
                                // If we have an execution result for this tool use, use it
                                if let Some(output) = tool_outputs.get(tool_use_id) {
                                    // Create a new ToolResult with updated content
                                    new_blocks.push(ContentBlock::ToolResult {
                                        tool_use_id: tool_use_id.clone(),
                                        content: output.clone(),
                                        is_error: *is_error,
                                        start_time: *start_time,
                                        end_time: *end_time,
                                    });
                                    need_update = true;
                                } else {
                                    // Keep the original block
                                    new_blocks.push(block.clone());
                                }
                            }
                            _ => {
                                // Keep other blocks as is
                                new_blocks.push(block.clone());
                            }
                        }
                    }

                    if need_update {
                        let mut updated = msg.clone();
                        updated.content = MessageContent::Structured(new_blocks);
                        messages.push(updated);
                    } else {
                        // No changes needed, use original message
                        messages.push(msg.clone());
                    }
                }
                MessageContent::Text(text) => {
                    if msg.is_compaction_summary {
                        let mut updated = msg.clone();
                        updated.content =
                            MessageContent::Text(Self::format_compaction_summary_for_prompt(text));
                        messages.push(updated);
                    } else {
                        messages.push(msg.clone());
                    }
                }
            }
        }

        messages
    }

    /// Executes a tool and catches all errors, returning them as Results
    async fn execute_tool(&mut self, tool_request: &ToolRequest) -> Result<bool> {
        debug!(
            "Executing tool request: {} (id: {})",
            tool_request.name, tool_request.id
        );

        // Check if this is a hidden tool
        let is_hidden = ToolRegistry::global().is_tool_hidden(&tool_request.name, self.tool_scope);

        // Handle name_session tool specially at agent level
        if tool_request.name == "name_session" {
            // Extract title from input
            if let Some(title) = tool_request.input["title"].as_str() {
                let title = title.trim();
                if !title.is_empty() {
                    trace!("Obtained session title from LLM: {}", title);
                    self.session_name = title.to_string();

                    // Create a successful tool execution record
                    let tool_execution = ToolExecution {
                        tool_request: tool_request.clone(),
                        result: Box::new(crate::tools::impls::name_session::NameSessionOutput {
                            title: title.to_string(),
                        }),
                    };
                    self.tool_executions.push(tool_execution);
                    return Ok(true); // Success, but don't show in UI
                } else {
                    warn!("Title for name_session is empty after trimming");
                }
            } else {
                warn!("No 'title' field found in name_session input or it's not a string");
            }

            // If we get here, the input was invalid
            return Err(anyhow::anyhow!("Invalid session title provided"));
        }

        // Update status to Running before execution (skip for hidden tools)
        if !is_hidden {
            self.ui
                .send_event(UiEvent::UpdateToolStatus {
                    tool_id: tool_request.id.clone(),
                    status: crate::ui::ToolStatus::Running,
                    message: None,
                    output: None,
                })
                .await?;
        }

        // Get the tool - could fail with UnknownTool
        let tool = match ToolRegistry::global().get(&tool_request.name) {
            Some(tool) => tool,
            None => return Err(ToolError::UnknownTool(tool_request.name.clone()).into()),
        };

        // Create a tool context
        let mut context = ToolContext {
            project_manager: self.project_manager.as_ref(),
            command_executor: self.command_executor.as_ref(),
            working_memory: Some(&mut self.working_memory),
            plan: Some(&mut self.plan),
            ui: Some(self.ui.as_ref()),
            tool_id: Some(tool_request.id.clone()),
            permission_handler: self.permission_handler.as_deref(),
        };

        // Execute the tool - could fail with ParseError or other errors
        let mut input = tool_request.input.clone();
        let result = match tool.invoke(&mut context, &mut input).await {
            Ok(result) => {
                // Tool executed successfully (but may have failed functionally)
                let success = result.is_success();

                // Check if input parameters were modified during execution
                let input_modified = input != tool_request.input;

                // Determine UI status based on result
                let status = if success {
                    crate::ui::ToolStatus::Success
                } else {
                    crate::ui::ToolStatus::Error
                };

                // Generate status string from result
                let short_output = result.as_render().status();

                // Generate isolated output from result
                let mut resources_tracker = ResourcesTracker::new();
                let output = result.as_render().render(&mut resources_tracker);

                // Update tool status with result (skip for hidden tools)
                if !is_hidden {
                    self.ui
                        .send_event(UiEvent::UpdateToolStatus {
                            tool_id: tool_request.id.clone(),
                            status,
                            message: Some(short_output),
                            output: Some(output),
                        })
                        .await?;
                }

                // Create the tool request with potentially updated input
                let final_tool_request = if input_modified {
                    debug!("Tool input was modified during execution");
                    ToolRequest {
                        id: tool_request.id.clone(),
                        name: tool_request.name.clone(),
                        input: input.clone(),
                        start_offset: tool_request.start_offset,
                        end_offset: tool_request.end_offset,
                    }
                } else {
                    tool_request.clone()
                };

                // Create and store the ToolExecution record
                let tool_execution = ToolExecution {
                    tool_request: final_tool_request.clone(),
                    result,
                };

                // Store the execution record
                self.tool_executions.push(tool_execution);

                // Update message history if input was modified
                if input_modified {
                    if !is_hidden {
                        self.notify_tool_parameter_updates(
                            &tool_request.input,
                            &input,
                            &tool_request.id,
                        )
                        .await?;
                    }

                    if let Err(e) =
                        self.update_message_history_with_formatted_tool(&final_tool_request)
                    {
                        warn!(
                            "Failed to update message history after input modification: {}",
                            e
                        );
                    }
                }

                Ok(success)
            }
            Err(e) => {
                // Tool execution failed (parameter error, etc.)
                let error_text = Self::format_error_for_user(&e);

                // Update UI status to error (skip for hidden tools)
                if !is_hidden {
                    self.ui
                        .send_event(UiEvent::UpdateToolStatus {
                            tool_id: tool_request.id.clone(),
                            status: crate::ui::ToolStatus::Error,
                            message: Some(error_text.clone()),
                            output: Some(error_text.clone()),
                        })
                        .await?;
                }

                // Create a ToolExecution record for the error
                let tool_execution = if let Some(tool_error) = e.downcast_ref::<ToolError>() {
                    match tool_error {
                        ToolError::ParseError(_) => {
                            // For parse errors, create a parse error execution
                            ToolExecution::create_parse_error(tool_request.id.clone(), error_text)
                        }
                        ToolError::UnknownTool(_) => {
                            // This shouldn't happen since we check above, but handle it
                            ToolExecution::create_parse_error(tool_request.id.clone(), error_text)
                        }
                    }
                } else {
                    // For other error types, also create a parse error record
                    ToolExecution::create_parse_error(tool_request.id.clone(), error_text)
                };

                // Store the execution record
                self.tool_executions.push(tool_execution);

                // Return the error to be handled by manage_tool_execution
                Err(e)
            }
        };

        result
    }

    async fn notify_tool_parameter_updates(
        &self,
        original: &serde_json::Value,
        updated: &serde_json::Value,
        tool_id: &str,
    ) -> Result<()> {
        let (Some(original_map), Some(updated_map)) = (original.as_object(), updated.as_object())
        else {
            return Ok(());
        };

        for (key, new_value) in updated_map {
            let old_value = original_map.get(key);
            if old_value == Some(new_value) {
                continue;
            }

            let value_str = if let Some(s) = new_value.as_str() {
                s.to_string()
            } else {
                new_value.to_string()
            };

            self.ui
                .send_event(UiEvent::UpdateToolParameter {
                    tool_id: tool_id.to_string(),
                    name: key.clone(),
                    value: value_str,
                })
                .await?;
        }

        Ok(())
    }

    /// Update message history to reflect formatted tool parameters
    fn update_message_history_with_formatted_tool(
        &mut self,
        updated_request: &ToolRequest,
    ) -> Result<()> {
        let tool_syntax = self.tool_syntax();
        // Find the most recent assistant message that contains the tool call
        for message in self.message_history.iter_mut().rev() {
            if message.role == MessageRole::Assistant {
                match &mut message.content {
                    MessageContent::Structured(blocks) => {
                        // Look for the ToolUse block with matching ID
                        for block in blocks {
                            if let ContentBlock::ToolUse {
                                id, name, input, ..
                            } = block
                            {
                                if *id == updated_request.id && *name == updated_request.name {
                                    *input = updated_request.input.clone();
                                    debug!("Updated tool call {} in message history", id);
                                    return Ok(());
                                }
                            }
                        }
                    }
                    MessageContent::Text(text) => {
                        // For text content, we need to update the tool call in the text
                        // This is more complex and depends on the tool syntax
                        if let Ok(updated_text) = Self::update_tool_call_in_text_static(
                            text,
                            updated_request,
                            tool_syntax,
                        ) {
                            *text = updated_text;
                            debug!("Updated tool call {} in text message", updated_request.id);
                            return Ok(());
                        }
                    }
                }
                // Only check the most recent assistant message
                break;
            }
        }

        warn!(
            "Could not find tool call {} to update in message history",
            updated_request.id
        );
        Ok(())
    }

    /// Static helper to update tool call in text (to avoid borrowing issues)
    pub fn update_tool_call_in_text_static(
        text: &str,
        updated_request: &ToolRequest,
        tool_syntax: ToolSyntax,
    ) -> Result<String> {
        use crate::tools::formatter::get_formatter;

        // Check if we have offset information for precise replacement
        if let (Some(start_offset), Some(end_offset)) =
            (updated_request.start_offset, updated_request.end_offset)
        {
            // Validate offsets are within bounds
            if start_offset <= text.len() && end_offset <= text.len() && start_offset <= end_offset
            {
                // Generate the new formatted tool call
                let formatter = get_formatter(tool_syntax);
                let new_tool_call = formatter.format_tool_request(updated_request)?;

                // Replace the tool block at the exact location
                let mut updated_text = String::new();
                updated_text.push_str(&text[..start_offset]);
                updated_text.push_str(&new_tool_call);
                updated_text.push_str(&text[end_offset..]);

                debug!(
                    "Replaced tool call {} at offsets {}..{} in text message",
                    updated_request.id, start_offset, end_offset
                );
                return Ok(updated_text);
            } else {
                warn!(
                    "Invalid offsets for tool call {}: start={}, end={}, text_len={}",
                    updated_request.id,
                    start_offset,
                    end_offset,
                    text.len()
                );
            }
        }

        // Fallback: append the updated tool call as a comment (for Native mode or when offsets are missing)
        let formatter = get_formatter(tool_syntax);
        let new_tool_call = formatter.format_tool_request(updated_request)?;

        let updated_text = format!(
            "{}\n\n<!-- Tool call {} was updated after auto-formatting -->\n{}",
            text, updated_request.id, new_tool_call
        );

        debug!(
            "Appended updated tool call {} to text message (fallback mode)",
            updated_request.id
        );
        Ok(updated_text)
    }
}
