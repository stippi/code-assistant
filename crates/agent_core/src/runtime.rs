//! The agent loop. Application behavior plugs in through the hook traits in
//! [`crate::hooks`]; application state travels type-erased in `extensions`.

use crate::dialect::ToolDialect;
use crate::hooks::{ContextSnapshot, HookRegistry, LoopCtx, RecoveryAction, ToolServicesProvider};
use crate::persistence::{AgentSnapshot, SnapshotPersistence};
use crate::tree::{ConversationPath, MessageNode, NodeId};
use crate::types::{ToolExecution, ToolRequest, text_summary_from_blocks, to_tool_definitions};
use crate::ui::{AgentActivity, AgentUi, AgentUiEvent, DisplayFragment, HiddenTools, UIError};
use anyhow::Result;
use command_executor::CommandExecutor;
use llm::{
    ContentBlock, LLMProvider, LLMRequest, Message, MessageContent, MessageRole, StreamingCallback,
    StreamingChunk, ToolResultContent, ToolResultImage,
};
use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tools_core::{
    PermissionMediator, ResourcesTracker, ToolContext, ToolError, ToolPermissions, ToolRegistry,
};
use tracing::{debug, trace, warn};

/// Everything an [`AgentRuntime`] is built from.
pub struct AgentRuntimeComponents {
    pub llm_provider: Box<dyn LLMProvider>,
    /// How tool calls travel between the LLM and the loop.
    pub dialect: Arc<dyn ToolDialect>,
    pub ui: Arc<dyn AgentUi>,
    /// The tools offered to the LLM and dispatched by the loop.
    pub registry: Arc<ToolRegistry>,
    /// Capability tag selecting the tool set within the registry.
    pub tool_capability: String,
    /// Which tool invocations the stream processors suppress in the UI.
    pub stream_hidden_tools: HiddenTools,
    pub command_executor: Arc<dyn CommandExecutor>,
    pub permission_handler: Option<Arc<dyn PermissionMediator>>,
    /// The active permission tier plus session-scoped grants; the loop gates
    /// every tool invocation on it before dispatching.
    pub permissions: ToolPermissions,
    /// Builds the application services handed to each tool invocation.
    pub services_provider: Arc<dyn ToolServicesProvider>,
    pub state_persistence: Box<dyn SnapshotPersistence>,
    pub hooks: HookRegistry,
    /// Application-specific loop state, exposed to the hooks type-erased.
    /// `Sync` because the loop holds `&self` across awaits.
    pub extensions: Box<dyn Any + Send + Sync>,
}

/// Defines control flow for the agent loop.
enum LoopFlow {
    /// Continue to the next iteration of the loop.
    Continue,
    /// Get user input and then continue the loop.
    GetUserInput,
}

pub struct AgentRuntime {
    hooks: HookRegistry,
    /// Application-specific loop state, exposed to the hooks type-erased via
    /// `LoopCtx::extensions` / `PromptCtx::extensions`.
    extensions: Box<dyn Any + Send + Sync>,
    llm_provider: Box<dyn LLMProvider>,
    /// How tool calls travel between the LLM and the loop.
    dialect: Arc<dyn ToolDialect>,
    registry: Arc<ToolRegistry>,
    tool_capability: String,
    stream_hidden_tools: HiddenTools,
    command_executor: Arc<dyn CommandExecutor>,
    ui: Arc<dyn AgentUi>,
    state_persistence: Box<dyn SnapshotPersistence>,
    /// Builds the application services handed to each tool invocation.
    services_provider: Arc<dyn ToolServicesProvider>,

    permission_handler: Option<Arc<dyn PermissionMediator>>,
    permissions: ToolPermissions,

    // ========================================================================
    // Branching: Tree-based message storage
    // ========================================================================
    /// All message nodes in the session (tree structure)
    message_nodes: BTreeMap<NodeId, MessageNode>,
    /// The currently active path through the tree
    active_path: ConversationPath,
    /// Counter for generating unique node IDs
    next_node_id: NodeId,

    // ========================================================================
    // Legacy: Linearized message history (derived from active_path)
    // ========================================================================
    /// Store all messages exchanged (kept in sync with active_path)
    message_history: Vec<Message>,

    // Store the history of tool executions
    tool_executions: Vec<ToolExecution>,
    // Cached system prompts keyed by model hint
    cached_system_prompts: HashMap<String, String>,
    // Optional model identifier used for prompt selection
    model_hint: Option<String>,
    // Counter for generating unique request IDs
    next_request_id: u64,
    // Session ID for this agent instance
    session_id: Option<String>,
    // Shared pending message with the embedding application (structured content blocks)
    pending_message_ref: Option<Arc<Mutex<Option<Vec<llm::ContentBlock>>>>>,
}

impl AgentRuntime {
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

    pub fn new(components: AgentRuntimeComponents) -> Self {
        let AgentRuntimeComponents {
            llm_provider,
            dialect,
            ui,
            registry,
            tool_capability,
            stream_hidden_tools,
            command_executor,
            permission_handler,
            permissions,
            services_provider,
            state_persistence,
            hooks,
            extensions,
        } = components;

        Self {
            hooks,
            extensions,
            llm_provider,
            dialect,
            registry,
            tool_capability,
            stream_hidden_tools,
            command_executor,
            ui,
            state_persistence,
            services_provider,
            permission_handler,
            permissions,
            // Branching tree structure
            message_nodes: BTreeMap::new(),
            active_path: Vec::new(),
            next_node_id: 1,
            // Linearized message history
            message_history: Vec::new(),
            tool_executions: Vec::new(),
            cached_system_prompts: HashMap::new(),
            next_request_id: 1, // Start from 1
            session_id: None,
            pending_message_ref: None,
            model_hint: None,
        }
    }

    /// Replace the dialect (e.g. after the embedding application reloaded a
    /// session with a different tool syntax).
    pub fn set_dialect(&mut self, dialect: Arc<dyn ToolDialect>) {
        self.dialect = dialect;
    }

    /// Set the capability tag selecting the tool set within the registry.
    pub fn set_tool_capability(&mut self, tool_capability: String) {
        self.tool_capability = tool_capability;
    }

    /// Set or clear the session identifier attached to persisted snapshots.
    pub fn set_session_id(&mut self, session_id: Option<String>) {
        self.session_id = session_id;
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// The application-specific loop state.
    pub fn extensions(&self) -> &(dyn Any + Send) {
        self.extensions.as_ref()
    }

    /// The application-specific loop state, mutably.
    pub fn extensions_mut(&mut self) -> &mut (dyn Any + Send) {
        self.extensions.as_mut()
    }

    /// Restore the conversation (tree, active path, id counter, linearized
    /// history) from persisted state.
    pub fn restore_conversation(
        &mut self,
        message_nodes: BTreeMap<NodeId, MessageNode>,
        active_path: ConversationPath,
        next_node_id: NodeId,
        messages: Vec<Message>,
    ) {
        self.message_nodes = message_nodes;
        self.active_path = active_path;
        self.next_node_id = next_node_id;
        self.message_history = messages;
    }

    /// Restore the tool execution records from persisted state.
    pub fn set_tool_executions(&mut self, tool_executions: Vec<ToolExecution>) {
        self.tool_executions = tool_executions;
    }

    /// Restore the request id counter from persisted state.
    pub fn set_next_request_id(&mut self, next_request_id: u64) {
        self.next_request_id = next_request_id;
    }

    /// Set the shared pending message reference from SessionInstance
    pub fn set_pending_message_ref(
        &mut self,
        pending_ref: Arc<Mutex<Option<Vec<llm::ContentBlock>>>>,
    ) {
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

    /// Get a reference to the message history
    pub fn message_history(&self) -> &[Message] {
        &self.message_history
    }

    /// Get and clear the pending message from shared state
    fn get_and_clear_pending_message(&self) -> Option<Vec<llm::ContentBlock>> {
        if let Some(ref pending_ref) = self.pending_message_ref {
            let mut pending = pending_ref.lock().ok()?;
            pending.take()
        } else {
            None
        }
    }

    /// Check if there is a pending message (without clearing it)
    fn has_pending_message(&self) -> bool {
        if let Some(ref pending_ref) = self.pending_message_ref {
            pending_ref.lock().ok().is_some_and(|p| p.is_some())
        } else {
            false
        }
    }

    /// Send a loop event to the UI.
    async fn send_ui(&self, event: AgentUiEvent) -> Result<(), UIError> {
        self.ui.send_event(event).await
    }

    /// Save the current state (message history and tool executions)
    fn save_state(&mut self) -> Result<()> {
        trace!(
            "saving {} messages to persistence (tree nodes: {})",
            self.message_history.len(),
            self.message_nodes.len()
        );

        let snapshot = AgentSnapshot {
            session_id: self.session_id.clone(),
            message_nodes: self.message_nodes.clone(),
            active_path: self.active_path.clone(),
            next_node_id: self.next_node_id,
            messages: self.message_history.clone(),
            tool_executions: self.tool_executions.clone(),
            next_request_id: self.next_request_id,
        };
        self.state_persistence
            .save(snapshot, self.extensions.as_ref())
    }

    /// Pre-allocate the next node_id without creating a node.
    /// The returned ID is guaranteed to be used by the next `append_message` call
    /// (or `append_message_with_node_id`).
    pub fn reserve_node_id(&mut self) -> NodeId {
        let id = self.next_node_id;
        self.next_node_id += 1;
        id
    }

    /// Adds a message to the history using a pre-allocated node_id.
    /// Use `reserve_node_id()` to obtain the ID before streaming starts,
    /// then call this after streaming completes.
    pub fn append_message_with_node_id(&mut self, message: Message, node_id: NodeId) -> Result<()> {
        let parent_id = self.active_path.last().copied();

        let node = MessageNode {
            id: node_id,
            message: message.clone(),
            parent_id,
            created_at: std::time::SystemTime::now(),
            extension: None,
        };

        self.message_nodes.insert(node_id, node);
        self.active_path.push(node_id);

        for observer in &self.hooks.observers {
            observer.on_message(self.session_id.as_deref(), &message);
        }

        // Also add to linearized history
        self.message_history.push(message);

        self.save_state()?;
        Ok(())
    }

    /// Adds a message to the history and saves the state.
    /// This adds the message to both the tree structure and the linearized history.
    /// Allocates a new node_id automatically.
    pub fn append_message(&mut self, message: Message) -> Result<()> {
        let node_id = self.reserve_node_id();
        self.append_message_with_node_id(message, node_id)
    }

    /// Run a single iteration of the agent loop without waiting for user input
    /// This is used in the new on-demand agent architecture
    pub async fn run_single_iteration(&mut self) -> Result<()> {
        let mut streaming_retry_count: u32 = 0;

        loop {
            // Check for pending user message and add it to history at start of each iteration
            if let Some(pending_blocks) = self.get_and_clear_pending_message() {
                let text_summary = text_summary_from_blocks(&pending_blocks);
                debug!("Processing pending user message: {}", text_summary);
                self.append_message(Message::new_user_content(pending_blocks))?;

                // Notify UI about the user message
                self.send_ui(AgentUiEvent::UserInputAppended {
                    content: text_summary,
                    node_id: None, // Pending messages don't have node_id yet
                })
                .await?;
            }

            if self.should_trigger_compaction()? {
                self.perform_compaction().await?;
                continue;
            }

            let messages = self.render_tool_results_in_messages();

            // Pre-allocate the node_id for this assistant message.
            // This is passed to the UI with StreamingStarted so the container
            // is tagged from the start, and then used in append_message_with_node_id
            // to guarantee the same ID is persisted.
            let reserved_node_id = self.reserve_node_id();

            // 1. Get LLM response (without adding to history yet)
            let (llm_response, request_id) = match self
                .get_next_assistant_message(messages, reserved_node_id)
                .await
            {
                Ok(result) => {
                    // Successful response — reset the retry counter
                    streaming_retry_count = 0;
                    result
                }
                // `continue` restarts the loop, which re-renders the messages and
                // retries get_next_assistant_message. (StreamingStopped was already
                // sent by get_next_assistant_message in its error path.)
                Err(e) => match self.hooks.recovery.classify(&e, streaming_retry_count) {
                    RecoveryAction::ReduceContext => {
                        self.recover_from_oversized_prompt().await?;
                        continue;
                    }
                    RecoveryAction::RetryStream {
                        delay,
                        attempt,
                        max_attempts,
                    } => {
                        streaming_retry_count = attempt;
                        self.prepare_streaming_retry(&e, attempt, max_attempts, delay)
                            .await;
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    RecoveryAction::Fail => return Err(e),
                },
            };

            // 2. Add original LLM response to message history using the pre-allocated node_id
            if !llm_response.content.is_empty() {
                self.append_message_with_node_id(
                    Message::new_assistant_content(llm_response.content.clone())
                        .with_request_id(request_id)
                        .with_usage(llm_response.usage.clone()),
                    reserved_node_id,
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
                if let Some(last_msg) = self.message_history.last_mut()
                    && last_msg.role == MessageRole::Assistant
                {
                    last_msg.content =
                        MessageContent::Structured(truncated_response.content.clone());
                    last_msg.usage = Some(truncated_response.usage.clone());
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

                        // Save state after tool executions
                        self.save_state()?;

                        match flow {
                            LoopFlow::Continue => { /* Continue to the next iteration */ }
                            LoopFlow::GetUserInput => {
                                // Complete iteration instead of waiting for input
                                debug!("Tool execution complete - waiting for next user message");
                                return Ok(());
                            }
                        }
                    }
                    // If tool_requests is empty with Continue flow, this means there was a parse error
                    // and we should continue the loop to give the LLM another chance to respond correctly
                }
            }
        }
    }

    /// Drop dangling assistant tool requests (no following tool result)
    /// from a freshly restored history.
    pub fn normalize_loaded_message_history(&mut self) {
        if self.message_history.is_empty() {
            return;
        }

        let dialect = self.dialect.clone();
        let mut removed = 0usize;

        while let Some(last_assistant_idx) = self
            .message_history
            .iter()
            .rposition(|message| message.role == MessageRole::Assistant)
        {
            let last_assistant = &self.message_history[last_assistant_idx];

            if !dialect.message_contains_invocation(last_assistant, self.registry.as_ref()) {
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
        match self.dialect.extract_requests(
            llm_response,
            request_counter,
            0,
            self.registry.as_ref(),
        ) {
            Ok((requests, truncated_response)) => {
                if requests.is_empty() && !self.has_pending_message() {
                    Ok((requests, LoopFlow::GetUserInput, truncated_response))
                } else {
                    Ok((requests, LoopFlow::Continue, truncated_response))
                }
            }
            Err(e) => {
                let error_text = Self::format_error_for_user(&e);

                let error_msg = if self.dialect.uses_native_tools() {
                    // For native mode, keep text message since parsing errors occur before
                    // we have any LLM-provided tool IDs to reference
                    Message::new_user(error_text)
                } else {
                    // For text dialects, create structured tool-result message like regular tool results
                    // Generate normal tool ID for consistency with UI expectations
                    let tool_id = format!("tool-{request_counter}-1");

                    // Create and store a ToolExecution for the parse error
                    let tool_execution =
                        ToolExecution::create_parse_error(tool_id.clone(), error_text.clone());
                    self.tool_executions.push(tool_execution);

                    Message::new_user_content(vec![ContentBlock::ToolResult {
                        tool_use_id: tool_id,
                        content: ToolResultContent::text(error_text),
                        is_error: Some(true),
                        start_time: Some(SystemTime::now()),
                        end_time: None,
                    }])
                };

                self.append_message(error_msg)?;
                // Return original response for error cases
                Ok((Vec::new(), LoopFlow::Continue, llm_response.clone())) // Continue without user input on parsing errors
            }
        }
    }

    /// Executes a list of tool requests and appends tool results to message history.
    /// Requests selected by the dispatch policy are executed concurrently.
    async fn manage_tool_execution(&mut self, tool_requests: &[ToolRequest]) -> Result<LoopFlow> {
        let parallel_indices = self.hooks.dispatch.parallel_indices(tool_requests);

        // Execute the policy-selected tools concurrently if we have multiple
        let parallel_results = if parallel_indices.len() > 1 {
            debug!("Running {} tools in parallel", parallel_indices.len());
            self.execute_tools_in_parallel(
                parallel_indices
                    .iter()
                    .map(|i| &tool_requests[*i])
                    .collect(),
            )
            .await
        } else {
            Vec::new()
        };

        // Build content blocks in original order
        let mut content_blocks: Vec<Option<ContentBlock>> = vec![None; tool_requests.len()];
        let mut parallel_result_iter = parallel_results.into_iter();

        // Process results in original order
        for (idx, tool_request) in tool_requests.iter().enumerate() {
            let result_block = if parallel_indices.len() > 1 && parallel_indices.contains(&idx) {
                // This request ran in parallel - get result from parallel execution

                parallel_result_iter.next().unwrap_or_else(|| {
                    let start_time = Some(SystemTime::now());
                    ContentBlock::ToolResult {
                        tool_use_id: tool_request.id.clone(),
                        content: ToolResultContent::text("Internal error: missing parallel result"),
                        is_error: Some(true),
                        start_time,
                        end_time: Some(SystemTime::now()),
                    }
                })
            } else {
                // Sequential execution
                let start_time = Some(SystemTime::now());
                match self.execute_tool(tool_request).await {
                    Ok(success) => ContentBlock::ToolResult {
                        tool_use_id: tool_request.id.clone(),
                        content: ToolResultContent::text(""),
                        is_error: if success { None } else { Some(true) },
                        start_time,
                        end_time: Some(SystemTime::now()),
                    },
                    Err(e) => {
                        let error_text = Self::format_error_for_user(&e);
                        ContentBlock::ToolResult {
                            tool_use_id: tool_request.id.clone(),
                            content: ToolResultContent::text(error_text),
                            is_error: Some(true),
                            start_time,
                            end_time: Some(SystemTime::now()),
                        }
                    }
                }
            };
            content_blocks[idx] = Some(result_block);
        }

        // Flatten and add message
        let final_blocks: Vec<_> = content_blocks.into_iter().flatten().collect();
        if !final_blocks.is_empty() {
            let result_message = Message::new_user_content(final_blocks);
            self.append_message(result_message)?;
        }
        Ok(LoopFlow::Continue)
    }

    /// Execute multiple tool requests in parallel.
    /// Returns ContentBlocks in the same order as input.
    async fn execute_tools_in_parallel(
        &mut self,
        tool_requests: Vec<&ToolRequest>,
    ) -> Vec<ContentBlock> {
        use futures::future::join_all;

        // Create futures for each tool request
        let futures: Vec<_> = tool_requests
            .iter()
            .map(|tool_request| {
                let request = (*tool_request).clone();
                let ui = self.ui.clone();
                let registry = self.registry.clone();
                let command_executor = self.command_executor.clone();
                let permission_handler = self.permission_handler.clone();
                let permissions = self.permissions.clone();
                let services_provider = self.services_provider.clone();
                let scope_tag = self.tool_capability.clone();
                let session_id = self.session_id.clone();

                async move {
                    let start_time = Some(SystemTime::now());

                    let (is_success, tool_execution) = Self::execute_tool_request_detached(
                        request,
                        ui,
                        registry,
                        command_executor,
                        permission_handler,
                        permissions,
                        services_provider,
                        scope_tag,
                        session_id,
                    )
                    .await;

                    let end_time = Some(SystemTime::now());

                    let content_block = ContentBlock::ToolResult {
                        tool_use_id: tool_execution.tool_request.id.clone(),
                        content: ToolResultContent::text(""),
                        is_error: if is_success { None } else { Some(true) },
                        start_time,
                        end_time,
                    };
                    (content_block, tool_execution)
                }
            })
            .collect();

        // Execute all in parallel
        let results = join_all(futures).await;

        // Collect results and tool executions
        let mut content_blocks = Vec::new();
        for (content_block, tool_execution) in results {
            debug!(
                "Parallel tool {} ({}) completed",
                tool_execution.tool_request.name, tool_execution.tool_request.id
            );
            self.tool_executions.push(tool_execution);
            content_blocks.push(content_block);
        }

        content_blocks
    }

    /// Execute a single tool request without exclusive access to the agent
    /// state, for the parallel branch decided by the dispatch policy.
    /// Compared to the sequential path, interceptors do not run, the plan is
    /// unavailable, and input modifications are not propagated back into the
    /// message history.
    #[allow(clippy::too_many_arguments)]
    async fn execute_tool_request_detached(
        tool_request: ToolRequest,
        ui: Arc<dyn AgentUi>,
        registry: Arc<ToolRegistry>,
        command_executor: Arc<dyn CommandExecutor>,
        permission_handler: Option<Arc<dyn PermissionMediator>>,
        permissions: ToolPermissions,
        services_provider: Arc<dyn ToolServicesProvider>,
        scope_tag: String,
        session_id: Option<String>,
    ) -> (bool, ToolExecution) {
        let is_hidden = registry.is_tool_hidden(&tool_request.name, &scope_tag);

        // Update UI to show running status (skip for hidden tools)
        if !is_hidden {
            let _ = ui
                .send_event(AgentUiEvent::UpdateToolStatus {
                    tool_id: tool_request.id.clone(),
                    status: crate::ui::ToolStatus::Running,
                    message: None,
                    output: None,
                    duration_seconds: None,
                    images: vec![],
                })
                .await;
        }

        let execution_start = std::time::Instant::now();

        let invoke_result = match registry.get(&tool_request.name) {
            None => Err(ToolError::UnknownTool(tool_request.name.clone()).into()),
            Some(_) if !registry.tool_has_capability(&tool_request.name, &scope_tag) => {
                Err(anyhow::anyhow!(
                    "Tool '{}' is not available in the current scope",
                    tool_request.name
                ))
            }
            Some(tool) => {
                // Tier-based permission gate, mirroring the sequential path.
                match permissions
                    .check(
                        permission_handler.as_deref(),
                        &tool.spec(),
                        Some(&tool_request.id),
                        &tool_request.input,
                    )
                    .await
                {
                    Err(e) => Err(e),
                    Ok(()) => {
                        let mut services = services_provider.detached(&tool_request.id);
                        let mut context = ToolContext {
                            command_executor: command_executor.as_ref(),
                            tool_id: Some(tool_request.id.clone()),
                            session_id,
                            permission_handler: permission_handler.as_deref(),
                            extensions: Some(services.as_mut()),
                        };
                        let mut input = tool_request.input.clone();
                        tool.invoke(&mut context, &mut input).await
                    }
                }
            }
        };

        let execution_duration = Some(execution_start.elapsed().as_secs_f64());

        match invoke_result {
            Ok(result) => {
                let success = result.is_success();
                let status = if success {
                    crate::ui::ToolStatus::Success
                } else {
                    crate::ui::ToolStatus::Error
                };

                let status_msg = result.as_render().status();
                let mut resources_tracker = ResourcesTracker::new();
                let ui_output = result.as_render().render_for_ui(&mut resources_tracker);
                let images = result.render_images();

                if !is_hidden {
                    let _ = ui
                        .send_event(AgentUiEvent::UpdateToolStatus {
                            tool_id: tool_request.id.clone(),
                            status,
                            message: Some(status_msg),
                            output: Some(ui_output),
                            duration_seconds: execution_duration,
                            images,
                        })
                        .await;
                }

                (
                    success,
                    ToolExecution {
                        tool_request,
                        result,
                    },
                )
            }
            Err(e) => {
                let error_text = Self::format_error_for_user(&e);

                if !is_hidden {
                    let _ = ui
                        .send_event(AgentUiEvent::UpdateToolStatus {
                            tool_id: tool_request.id.clone(),
                            status: crate::ui::ToolStatus::Error,
                            message: Some(error_text.clone()),
                            output: Some(error_text.clone()),
                            duration_seconds: execution_duration,
                            images: vec![],
                        })
                        .await;
                }

                (
                    false,
                    ToolExecution::create_parse_error(tool_request.id, error_text),
                )
            }
        }
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

        let ctx = crate::hooks::PromptCtx {
            dialect: self.dialect.as_ref(),
            model_hint: self.model_hint.as_deref(),
            session_id: self.session_id.as_deref(),
            registry: self.registry.as_ref(),
            extensions: self.extensions.as_ref(),
        };
        let system_message = self.hooks.system_prompt.build(&ctx);

        // Cache the system message for the current model hint
        self.cached_system_prompts
            .insert(cache_key, system_message.clone());

        system_message
    }

    /// Invalidate the cached system message to force regeneration
    pub fn invalidate_system_message_cache(&mut self) {
        self.cached_system_prompts.clear();
    }

    /// Convert ToolResult blocks to Text blocks for custom tool-syntax mode
    fn convert_tool_results_to_text(&self, messages: Vec<Message>) -> Vec<Message> {
        // Create a fresh ResourcesTracker for rendering
        let mut resources_tracker = ResourcesTracker::new();

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
                            volatile: msg.volatile,
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

    /// Runs the iteration hooks over the rendered messages right before they
    /// are sent to the LLM (e.g. to inject system reminders).
    pub fn shape_request_messages(&mut self, mut messages: Vec<Message>) -> Vec<Message> {
        let ctx = LoopCtx {
            tool_executions: &mut self.tool_executions,
            message_nodes: &mut self.message_nodes,
            active_path: &self.active_path,
            session_id: self.session_id.as_deref(),
            registry: self.registry.as_ref(),
            extensions: self.extensions.as_mut(),
        };
        for hook in &self.hooks.iteration_hooks {
            if let Err(e) = hook.shape_request(&mut messages, &ctx) {
                warn!("Iteration hook failed to shape the request: {}", e);
            }
        }
        messages
    }

    /// Gets the next assistant message from the LLM provider.
    /// `node_id` is the pre-allocated persistence node ID for this assistant message,
    /// sent to the UI with `StreamingStarted` so the container is tagged from the start.
    async fn get_next_assistant_message(
        &mut self,
        messages: Vec<Message>,
        node_id: NodeId,
    ) -> Result<(llm::LLMResponse, u64)> {
        // Generate and increment request ID
        let request_id = self.next_request_id;
        self.next_request_id += 1;

        // Inform UI that a new LLM request is starting
        self.send_ui(AgentUiEvent::StreamingStarted {
            request_id,
            node_id,
        })
        .await?;
        debug!(
            "Starting LLM request with ID: {}, node_id: {}",
            request_id, node_id
        );

        let messages_with_reminder = self.shape_request_messages(messages);

        // Convert messages based on the dialect:
        // native tool calling keeps ToolUse blocks, text dialects convert to text
        let converted_messages = if self.dialect.uses_native_tools() {
            messages_with_reminder
        } else {
            self.convert_tool_results_to_text(messages_with_reminder)
        };

        let request = LLMRequest {
            messages: converted_messages,
            system_prompt: self.get_system_prompt(),
            tools: if self.dialect.uses_native_tools() {
                Some(to_tool_definitions(
                    self.registry
                        .as_ref()
                        .get_tool_definitions_with_capability(self.tool_capability.as_str()),
                ))
            } else {
                None
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
        let hidden_tools = self.stream_hidden_tools.clone();
        let processor = Arc::new(Mutex::new(self.dialect.stream_processor(
            self.ui.clone(),
            request_id,
            hidden_tools,
            self.registry.clone(),
        )));

        let ui_for_callback = self.ui.clone();
        let streaming_callback: StreamingCallback = Box::new(move |chunk: &StreamingChunk| {
            // Check if streaming should continue
            if !ui_for_callback.should_streaming_continue() {
                debug!("Streaming should stop - user requested cancellation");
                return Err(anyhow::anyhow!("Streaming cancelled by user"));
            }

            let mut processor_guard = processor
                .lock()
                .map_err(|e| anyhow::anyhow!("Stream processor mutex poisoned: {e}"))?;
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
                        .send_ui(AgentUiEvent::StreamingStopped {
                            request_id,
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
                    .send_ui(AgentUiEvent::StreamingStopped {
                        request_id,
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
            .send_ui(AgentUiEvent::StreamingStopped {
                request_id,
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

        let messages_with_reminder = self.shape_request_messages(messages);

        let converted_messages = if self.dialect.uses_native_tools() {
            messages_with_reminder
        } else {
            self.convert_tool_results_to_text(messages_with_reminder)
        };

        let request = LLMRequest {
            messages: converted_messages,
            system_prompt: self.get_system_prompt(),
            tools: if self.dialect.uses_native_tools() {
                Some(to_tool_definitions(
                    self.registry
                        .as_ref()
                        .get_tool_definitions_with_capability(self.tool_capability.as_str()),
                ))
            } else {
                None
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

    fn context_usage_ratio(&mut self) -> Result<Option<f32>> {
        let Some(limit) = self
            .hooks
            .compaction
            .context_limit(self.extensions.as_ref())?
        else {
            return Ok(None);
        };

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
        let snapshot = ContextSnapshot {
            usage_ratio: self.context_usage_ratio()?,
        };
        Ok(self.hooks.compaction.should_compact(&snapshot))
    }

    /// Shrinks the conversation after the provider rejected the prompt as too long.
    /// Replaces large tool results with error placeholders when possible — the next
    /// render of the message history then produces a much smaller prompt. If nothing
    /// is large enough to replace, drops the last assistant+tool-result exchange and
    /// forces context compaction as a last resort.
    async fn recover_from_oversized_prompt(&mut self) -> Result<()> {
        warn!("Prompt too long error detected, replacing large tool results with error messages");
        let replaced = self.replace_large_tool_results();
        if replaced.is_empty() {
            warn!(
                "No large tool results to replace — dropping last exchange and forcing compaction"
            );
            self.drop_last_tool_exchange();
            return self.perform_compaction().await;
        }
        // Notify the UI that these tools switched from success → error
        for (tool_id, error_message) in &replaced {
            let _ = self
                .send_ui(AgentUiEvent::UpdateToolStatus {
                    tool_id: tool_id.clone(),
                    status: crate::ui::ToolStatus::Error,
                    message: Some("Prompt Too Long".to_string()),
                    output: Some(error_message.clone()),
                    duration_seconds: None,
                    images: vec![],
                })
                .await;
        }
        Ok(())
    }

    /// Informs the user that a transient streaming failure is being retried and
    /// tells the UI to discard all partial content from the failed request.
    async fn prepare_streaming_retry(
        &self,
        error: &anyhow::Error,
        attempt: u32,
        max_attempts: u32,
        delay: std::time::Duration,
    ) {
        warn!(
            "Transient streaming error (attempt {}/{}), retrying in {:?}: {}",
            attempt, max_attempts, delay, error
        );

        // get_next_assistant_message already sent StreamingStopped{error: ...} for
        // the failed request, so the UI knows streaming ended. Now we tell it to
        // also remove whatever was already rendered.
        let _ = self
            .send_ui(AgentUiEvent::RollbackStreaming {
                request_id: self.next_request_id - 1,
            })
            .await;

        let _ = self
            .send_ui(AgentUiEvent::ShowTransientStatus {
                message: format!(
                    "Stream interrupted — retrying ({}/{})\u{2026}",
                    attempt, max_attempts
                ),
            })
            .await;
    }

    /// Replace the largest tool execution results **from the most recent turn**
    /// with [`PromptTooLongError`] placeholders so that the next LLM request has a
    /// chance to succeed.
    ///
    /// Returns a vec of `(tool_id, error_message)` for each replaced result,
    /// empty if nothing was replaced.  The caller is responsible for sending
    /// `UpdateToolStatus` UI events for these.
    fn replace_large_tool_results(&mut self) -> Vec<(String, String)> {
        use crate::types::PromptTooLongError;

        // Collect tool_use_ids from the last user message that contains ToolResult
        // blocks — these are the results from the most recent turn.
        let current_turn_ids: std::collections::HashSet<String> = self
            .message_history
            .iter()
            .rev()
            .find_map(|msg| {
                if msg.role != MessageRole::User {
                    return None;
                }
                if let MessageContent::Structured(blocks) = &msg.content {
                    let ids: Vec<String> = blocks
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::ToolResult { tool_use_id, .. } = b {
                                Some(tool_use_id.clone())
                            } else {
                                None
                            }
                        })
                        .collect();
                    if ids.is_empty() { None } else { Some(ids) }
                } else {
                    None
                }
            })
            .unwrap_or_default()
            .into_iter()
            .collect();

        if current_turn_ids.is_empty() {
            return Vec::new();
        }

        // Render each current-turn tool output to measure its size
        let mut sizes: Vec<(usize, usize)> = Vec::new(); // (index, byte_size)
        let mut tracker = ResourcesTracker::new();
        for (i, exec) in self.tool_executions.iter().enumerate() {
            if !current_turn_ids.contains(&exec.tool_request.id) {
                continue;
            }
            let rendered = exec.result.as_render().render(&mut tracker);
            sizes.push((i, rendered.len()));
        }

        // Sort descending by size
        sizes.sort_by_key(|item| std::cmp::Reverse(item.1));

        // Replace results that are above a minimum threshold (50KB) — there is no
        // point replacing tiny results since they are unlikely to be the cause.
        const MIN_REPLACE_THRESHOLD: usize = 50 * 1024;
        let mut replaced: Vec<(String, String)> = Vec::new();

        for (idx, byte_size) in sizes {
            if byte_size < MIN_REPLACE_THRESHOLD {
                break;
            }
            let tool_name = self.tool_executions[idx].tool_request.name.clone();
            let tool_id = self.tool_executions[idx].tool_request.id.clone();
            warn!(
                "Replacing tool result for '{}' ({}KB) with prompt-too-long error",
                tool_name,
                byte_size / 1024
            );
            let error = PromptTooLongError::new(&tool_name, byte_size);
            let error_message = error.error_message.clone();
            self.tool_executions[idx].result = Box::new(error);
            replaced.push((tool_id, error_message));
        }

        // Also update the corresponding ToolResult content blocks in message history
        // so the is_error flag is set correctly
        if !replaced.is_empty() {
            let replaced_ids: std::collections::HashSet<&str> =
                replaced.iter().map(|(id, _)| id.as_str()).collect();

            for msg in &mut self.message_history {
                if let MessageContent::Structured(blocks) = &mut msg.content {
                    for block in blocks {
                        if let ContentBlock::ToolResult {
                            tool_use_id,
                            is_error,
                            ..
                        } = block
                            && replaced_ids.contains(tool_use_id.as_str())
                        {
                            *is_error = Some(true);
                        }
                    }
                }
            }
        }

        replaced
    }

    /// Drop the last assistant → tool-result message pair from history.
    /// Also removes the corresponding `tool_executions` entries.
    /// Used as a last-resort fallback before forcing compaction when the prompt
    /// is too long but no individual tool result is large enough to replace.
    fn drop_last_tool_exchange(&mut self) {
        // Walk backwards to find the last user message with ToolResult blocks
        // and the assistant message immediately before it.
        let mut tool_result_idx = None;
        for i in (0..self.message_history.len()).rev() {
            let msg = &self.message_history[i];
            if msg.role == MessageRole::User
                && let MessageContent::Structured(blocks) = &msg.content
                && blocks
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
            {
                tool_result_idx = Some(i);
                break;
            }
        }

        let Some(tr_idx) = tool_result_idx else {
            return;
        };

        // Collect the tool_use_ids we're about to drop so we can clean up
        // tool_executions too.
        let mut dropped_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let MessageContent::Structured(blocks) = &self.message_history[tr_idx].content {
            for block in blocks {
                if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                    dropped_ids.insert(tool_use_id.clone());
                }
            }
        }

        // Remove the tool-result user message
        self.message_history.remove(tr_idx);

        // If the message right before it was the assistant message with the
        // corresponding ToolUse blocks, remove that too.
        if tr_idx > 0 {
            let prev = &self.message_history[tr_idx - 1];
            if prev.role == MessageRole::Assistant {
                self.message_history.remove(tr_idx - 1);
            }
        }

        // Remove corresponding tool executions
        self.tool_executions
            .retain(|e| !dropped_ids.contains(&e.tool_request.id));

        debug!(
            "Dropped last tool exchange ({} tool result(s)) from history",
            dropped_ids.len()
        );
    }

    async fn perform_compaction(&mut self) -> Result<()> {
        debug!("Starting context compaction");

        let compaction_message = Message {
            role: MessageRole::User,
            content: MessageContent::Text(self.hooks.compaction.compaction_prompt().to_string()),
            ..Default::default()
        };

        let mut messages = self.render_tool_results_in_messages();
        messages.push(compaction_message);
        self.send_ui(AgentUiEvent::ActivityChanged {
            activity: AgentActivity::WaitingForResponse,
        })
        .await?;
        let response_result = self.get_non_streaming_response(messages).await;
        self.send_ui(AgentUiEvent::ActivityChanged {
            activity: AgentActivity::Running,
        })
        .await?;
        let (response, _) = response_result?;

        let summary_text = Self::extract_compaction_summary_text(&response.content);

        // The compaction policy may contribute an addendum to the summary
        // message — e.g. reminding the model which skills it had loaded, since
        // those tool results are now gone from the trimmed history. It is kept
        // inside the summary message (not a separate message) to avoid two
        // consecutive user messages, but is excluded from the UI divider.
        let addendum = self
            .hooks
            .compaction
            .post_compaction_summary_addendum(self.extensions.as_ref());
        let summary_content = match &addendum {
            Some(addendum) if !addendum.trim().is_empty() => {
                format!("{summary_text}\n\n{addendum}")
            }
            _ => summary_text.clone(),
        };

        let summary_message = Message {
            role: MessageRole::User,
            content: MessageContent::Text(summary_content),
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

    /// Prepare messages for LLM request, dynamically rendering tool outputs.
    ///
    /// This function also handles cancelled tool executions: if an assistant message
    /// contains `ToolUse` blocks but there's no corresponding `ToolResult` in the
    /// following user message (or no following user message at all), we generate
    /// a synthetic "user cancelled" `ToolResult` to satisfy the API requirement that
    /// every `tool_use` must have a corresponding `tool_result`.
    pub fn render_tool_results_in_messages(&self) -> Vec<Message> {
        // Start with a clean slate
        let mut messages = Vec::new();

        // Create a fresh ResourcesTracker for this rendering pass
        let mut resources_tracker = ResourcesTracker::new();

        // First, collect all tool executions and build a map from tool_use_id to rendered output
        let mut tool_outputs = std::collections::HashMap::new();
        // Collect image data from tools that produce visual output
        let mut tool_images: std::collections::HashMap<String, Vec<tools_core::ImageData>> =
            std::collections::HashMap::new();

        // Process tool executions in reverse chronological order (newest first)
        // so newer tool calls take precedence in resource conflicts
        for execution in self.tool_executions.iter().rev() {
            let tool_use_id = &execution.tool_request.id;
            let rendered_output = execution.result.as_render().render(&mut resources_tracker);
            tool_outputs.insert(tool_use_id.clone(), rendered_output);

            // Collect any image data from the tool output
            let images = execution.result.render_images();
            if !images.is_empty() {
                tool_images.insert(tool_use_id.clone(), images);
            }
        }

        // Build a set of all tool_use_ids that have corresponding tool_results in the message history
        let mut tool_ids_with_results: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for msg in self.active_messages() {
            if let MessageContent::Structured(blocks) = &msg.content {
                for block in blocks {
                    if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                        tool_ids_with_results.insert(tool_use_id.clone());
                    }
                }
            }
        }

        // Now rebuild the message history, replacing tool outputs with our dynamically rendered versions
        let active_msgs: Vec<_> = self.active_messages().to_vec();
        for (idx, msg) in active_msgs.iter().enumerate() {
            match &msg.content {
                MessageContent::Structured(blocks) => {
                    if msg.role == MessageRole::Assistant {
                        // Check for ToolUse blocks that need synthetic ToolResults
                        let tool_use_ids: Vec<String> = blocks
                            .iter()
                            .filter_map(|block| {
                                if let ContentBlock::ToolUse { id, .. } = block {
                                    Some(id.clone())
                                } else {
                                    None
                                }
                            })
                            .collect();

                        // Find tool_use_ids without corresponding tool_results
                        let missing_results: Vec<&String> = tool_use_ids
                            .iter()
                            .filter(|id| !tool_ids_with_results.contains(*id))
                            .collect();

                        if !missing_results.is_empty() {
                            // We need to add the assistant message first, then add a synthetic
                            // user message with cancelled tool results
                            messages.push(msg.clone());

                            // Generate synthetic ToolResult blocks for cancelled tools
                            let cancelled_blocks: Vec<ContentBlock> = missing_results
                                .iter()
                                .map(|tool_id| {
                                    debug!(
                                        "Generating synthetic 'cancelled' tool result for tool_use_id: {}",
                                        tool_id
                                    );

                                    ContentBlock::ToolResult {
                                        tool_use_id: (*tool_id).clone(),
                                        content: ToolResultContent::text(
                                            "Tool execution was cancelled by user.",
                                        ),
                                        is_error: Some(true),
                                        start_time: None,
                                        end_time: None,
                                    }
                                })
                                .collect();

                            // Check if the next message is already a user message with tool results
                            // In that case, we need to merge the cancelled results
                            let next_msg = active_msgs.get(idx + 1);
                            let should_create_new_message = match next_msg {
                                Some(next) if next.role == MessageRole::User => {
                                    // Check if this user message has tool results
                                    match &next.content {
                                        MessageContent::Structured(next_blocks) => !next_blocks
                                            .iter()
                                            .any(|b| matches!(b, ContentBlock::ToolResult { .. })),
                                        _ => true,
                                    }
                                }
                                _ => true,
                            };

                            if should_create_new_message {
                                // Insert a new user message with the cancelled tool results
                                let cancelled_msg =
                                    Message::new_user_content(cancelled_blocks.clone());
                                messages.push(cancelled_msg);
                            }
                            // If next message already has tool results, we'll handle merging when we process it
                            continue;
                        }
                    }

                    // Look for ToolResult blocks and update with rendered output.
                    // When a tool produces images, they are embedded inside the
                    // ToolResultContent so Anthropic receives them in the
                    // `tool_result.content` array (per the API spec).
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
                                    // Build content with optional images
                                    let content = if let Some(images) = tool_images.get(tool_use_id)
                                    {
                                        ToolResultContent::with_images(
                                            output.clone(),
                                            images
                                                .iter()
                                                .map(|img| ToolResultImage {
                                                    media_type: img.media_type.clone(),
                                                    base64_data: img.base64_data.clone(),
                                                })
                                                .collect(),
                                        )
                                    } else {
                                        ToolResultContent::text(output.clone())
                                    };

                                    new_blocks.push(ContentBlock::ToolResult {
                                        tool_use_id: tool_use_id.clone(),
                                        content,
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
    /// Gives the registered interceptors a chance to handle the request
    /// before the standard dispatch. Returns `Some(result)` when one did.
    fn intercept_tool(&mut self, tool_request: &ToolRequest) -> Option<Result<bool>> {
        let mut ctx = LoopCtx {
            tool_executions: &mut self.tool_executions,
            message_nodes: &mut self.message_nodes,
            active_path: &self.active_path,
            session_id: self.session_id.as_deref(),
            registry: self.registry.as_ref(),
            extensions: self.extensions.as_mut(),
        };
        for interceptor in &self.hooks.interceptors {
            if let Some(result) = interceptor.try_intercept(tool_request, &mut ctx) {
                return Some(result);
            }
        }
        None
    }

    /// Notifies the registered interceptors that a tool executed successfully.
    fn after_tool_success(&mut self, tool_request: &ToolRequest) {
        let mut ctx = LoopCtx {
            tool_executions: &mut self.tool_executions,
            message_nodes: &mut self.message_nodes,
            active_path: &self.active_path,
            session_id: self.session_id.as_deref(),
            registry: self.registry.as_ref(),
            extensions: self.extensions.as_mut(),
        };
        for interceptor in &self.hooks.interceptors {
            interceptor.after_tool_success(tool_request, &mut ctx);
        }
    }

    /// A tool may rewrite its own input while executing (e.g. format-on-save).
    /// Propagates the updated input to the UI and rewrites the originating tool
    /// call in the message history so that follow-up requests see the final input.
    async fn propagate_modified_tool_input(
        &mut self,
        original_request: &ToolRequest,
        final_request: &ToolRequest,
        is_hidden: bool,
    ) -> Result<()> {
        if !is_hidden {
            self.notify_tool_parameter_updates(
                &original_request.input,
                &final_request.input,
                &original_request.id,
            )
            .await?;
        }

        if let Err(e) = self.update_message_history_with_formatted_tool(final_request) {
            warn!(
                "Failed to update message history after input modification: {}",
                e
            );
        }
        Ok(())
    }

    async fn execute_tool(&mut self, tool_request: &ToolRequest) -> Result<bool> {
        debug!(
            "Executing tool request: {} (id: {})",
            tool_request.name, tool_request.id
        );

        if let Some(result) = self.intercept_tool(tool_request) {
            return result;
        }

        // Check if this is a hidden tool
        let is_hidden = self
            .registry
            .as_ref()
            .is_tool_hidden(&tool_request.name, self.tool_capability.as_str());

        // Update status to Running before execution (skip for hidden tools)
        if !is_hidden {
            self.send_ui(AgentUiEvent::UpdateToolStatus {
                tool_id: tool_request.id.clone(),
                status: crate::ui::ToolStatus::Running,
                message: None,
                output: None,
                duration_seconds: None,
                images: vec![],
            })
            .await?;
        }

        // Get the tool - could fail with UnknownTool
        let tool = match self.registry.as_ref().get(&tool_request.name) {
            Some(tool) => tool,
            None => return Err(ToolError::UnknownTool(tool_request.name.clone()).into()),
        };

        // Verify the tool is allowed in the current scope.
        // The scope filtering on the tool list offered to the LLM is not sufficient on its own,
        // because models may hallucinate tool calls they know from training even when the tool
        // is not in the provided tool list (e.g. a sub-agent calling write_file).
        if !self
            .registry
            .as_ref()
            .tool_has_capability(&tool_request.name, self.tool_capability.as_str())
        {
            return Err(anyhow::anyhow!(
                "Tool '{}' is not available in the current scope",
                tool_request.name
            ));
        }

        // Tier-based permission gate: ask the user before dispatching when
        // the active tier requires it for this tool.
        if let Err(e) = self
            .permissions
            .check(
                self.permission_handler.as_deref(),
                &tool.spec(),
                Some(&tool_request.id),
                &tool_request.input,
            )
            .await
        {
            let error_text = Self::format_error_for_user(&e);
            if !is_hidden {
                self.send_ui(AgentUiEvent::UpdateToolStatus {
                    tool_id: tool_request.id.clone(),
                    status: crate::ui::ToolStatus::Error,
                    message: Some(error_text.clone()),
                    output: Some(error_text.clone()),
                    duration_seconds: None,
                    images: vec![],
                })
                .await?;
            }
            self.tool_executions.push(ToolExecution::create_parse_error(
                tool_request.id.clone(),
                error_text,
            ));
            return Err(e);
        }

        // Create a tool context. The services provider builds the application
        // extension for this invocation (state such as the plan may move in
        // for the duration) and takes it back afterwards.
        let mut services = self
            .services_provider
            .begin(self.extensions.as_mut(), &tool_request.id);
        let mut context = ToolContext {
            command_executor: self.command_executor.as_ref(),
            tool_id: Some(tool_request.id.clone()),
            session_id: self.session_id.clone(),
            permission_handler: self.permission_handler.as_deref(),
            extensions: Some(services.as_mut()),
        };

        // Execute the tool - could fail with ParseError or other errors
        let mut input = tool_request.input.clone();
        let execution_start = std::time::Instant::now();

        let invoke_result = tool.invoke(&mut context, &mut input).await;
        drop(context);
        self.services_provider
            .end(self.extensions.as_mut(), services);

        match invoke_result {
            Ok(result) => {
                let execution_duration = Some(execution_start.elapsed().as_secs_f64());

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

                // Generate output for UI display (may differ from LLM output for some tools)
                let mut resources_tracker = ResourcesTracker::new();
                let ui_output = result.as_render().render_for_ui(&mut resources_tracker);

                // Collect image data from tools that produce visual output
                let images = result.render_images();

                // Update tool status with result (skip for hidden tools)
                if !is_hidden {
                    self.send_ui(AgentUiEvent::UpdateToolStatus {
                        tool_id: tool_request.id.clone(),
                        status,
                        message: Some(short_output),
                        output: Some(ui_output),
                        duration_seconds: execution_duration,
                        images,
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

                if success {
                    self.after_tool_success(tool_request);
                }

                if input_modified {
                    self.propagate_modified_tool_input(
                        tool_request,
                        &final_tool_request,
                        is_hidden,
                    )
                    .await?;
                }

                Ok(success)
            }

            Err(e) => {
                let execution_duration = Some(execution_start.elapsed().as_secs_f64());

                // Tool execution failed (parameter error, etc.)
                let error_text = Self::format_error_for_user(&e);

                // Update UI status to error (skip for hidden tools)
                if !is_hidden {
                    self.send_ui(AgentUiEvent::UpdateToolStatus {
                        tool_id: tool_request.id.clone(),
                        status: crate::ui::ToolStatus::Error,
                        message: Some(error_text.clone()),
                        output: Some(error_text.clone()),
                        duration_seconds: execution_duration,
                        images: vec![],
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
        }
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

            warn!(
                "Agent format-on-save parameter update: tool_id='{}', param='{}', value_len={} ",
                tool_id,
                key,
                value_str.len()
            );

            self.send_ui(AgentUiEvent::UpdateToolParameter {
                tool_id: tool_id.to_string(),
                name: key.clone(),
                value: value_str,
                replace: true,
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
        let dialect = self.dialect.clone();
        let registry = self.registry.clone();
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
                                && *id == updated_request.id
                                && *name == updated_request.name
                            {
                                *input = updated_request.input.clone();
                                debug!("Updated tool call {} in message history", id);
                                return Ok(());
                            }
                        }
                    }
                    MessageContent::Text(text) => {
                        // For text content, we need to update the tool call in the text
                        // This is more complex and depends on the tool syntax
                        if let Ok(updated_text) = Self::update_tool_call_in_text_static(
                            text,
                            updated_request,
                            dialect.as_ref(),
                            registry.as_ref(),
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
        dialect: &dyn ToolDialect,
        registry: &ToolRegistry,
    ) -> Result<String> {
        // Check if we have offset information for precise replacement
        if let (Some(start_offset), Some(end_offset)) =
            (updated_request.start_offset, updated_request.end_offset)
        {
            // Validate offsets are within bounds and on character boundaries
            if start_offset <= text.len()
                && end_offset <= text.len()
                && start_offset <= end_offset
                && text.is_char_boundary(start_offset)
                && text.is_char_boundary(end_offset)
            {
                // Generate the new formatted tool call
                let new_tool_call = dialect.format_tool_request(updated_request, registry)?;

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
        let new_tool_call = dialect.format_tool_request(updated_request, registry)?;

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
