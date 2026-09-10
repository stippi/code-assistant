//! The agent loop. Application behavior plugs in through the hook traits in
//! [`crate::hooks`]; application state travels type-erased in `extensions`.

#[cfg(test)]
mod tests;
mod tool_execution;

use crate::dialect::ToolDialect;
use crate::execution::ToolJournal;
use crate::hooks::{ContextSnapshot, HookRegistry, LoopCtx, RecoveryAction, ToolServicesProvider};
use crate::persistence::{AgentCheckpoint, CheckpointPersistence};
use crate::tree::{Conversation, ConversationPath, MessageNode, NodeId};
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
    /// Capability tags whose tools are excluded for this run (e.g. the
    /// `scope:mcp-<server>` tags of MCP servers the session deactivated).
    /// A tool carrying any of these is neither offered nor dispatched.
    pub excluded_tool_capabilities: Vec<String>,
    /// Which tool invocations the stream processors suppress in the UI.
    pub stream_hidden_tools: HiddenTools,
    pub command_executor: Arc<dyn CommandExecutor>,
    pub permission_handler: Option<Arc<dyn PermissionMediator>>,
    /// The active permission tier plus session-scoped grants; the loop gates
    /// every tool invocation on it before dispatching.
    pub permissions: ToolPermissions,
    /// Builds the application services handed to each tool invocation.
    pub services_provider: Arc<dyn ToolServicesProvider>,
    pub state_persistence: Box<dyn CheckpointPersistence>,
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

/// Purely derived request adjustments; canonical state is never edited here.
#[derive(Default)]
struct PromptProjection {
    omitted_nodes: std::collections::HashSet<NodeId>,
    tool_results: HashMap<String, String>,
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
    excluded_tool_capabilities: Vec<String>,
    stream_hidden_tools: HiddenTools,
    command_executor: Arc<dyn CommandExecutor>,
    ui: Arc<dyn AgentUi>,
    state_persistence: Box<dyn CheckpointPersistence>,
    /// Builds the application services handed to each tool invocation.
    services_provider: Arc<dyn ToolServicesProvider>,

    permission_handler: Option<Arc<dyn PermissionMediator>>,
    permissions: ToolPermissions,
    cancellation: tools_core::RunCancellation,

    conversation: Conversation,
    /// Run-local LLM projection. Never included in a checkpoint.
    prompt_projection: PromptProjection,

    /// Every tool call of the conversation with its outcome.
    journal: ToolJournal,
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
            excluded_tool_capabilities,
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
            excluded_tool_capabilities,
            stream_hidden_tools,
            command_executor,
            ui,
            state_persistence,
            services_provider,
            permission_handler,
            permissions,
            cancellation: tools_core::RunCancellation::default(),
            conversation: Conversation::default(),
            prompt_projection: PromptProjection::default(),
            journal: ToolJournal::default(),
            cached_system_prompts: HashMap::new(),
            next_request_id: 1, // Start from 1
            session_id: None,
            pending_message_ref: None,
            model_hint: None,
        }
    }

    pub fn set_cancellation(&mut self, cancellation: tools_core::RunCancellation) {
        self.permissions.set_cancellation(cancellation.clone());
        self.cancellation = cancellation;
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

    /// Restore the conversation (tree, active path, id counter) from
    /// persisted state. `messages` is the legacy linear history, imported
    /// only when the session has no tree yet.
    pub fn restore_conversation(
        &mut self,
        message_nodes: BTreeMap<NodeId, MessageNode>,
        active_path: ConversationPath,
        next_node_id: NodeId,
        messages: Vec<Message>,
    ) {
        self.conversation =
            Conversation::restore(message_nodes, active_path, next_node_id, messages);
        self.prompt_projection = PromptProjection::default();
    }

    /// Restore the tool execution records from persisted state.
    pub fn set_tool_executions(&mut self, tool_executions: Vec<ToolExecution>) {
        self.journal = ToolJournal::restore(tool_executions);
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

    /// The messages on the active path, in order.
    pub fn message_history(&self) -> Vec<Message> {
        self.conversation.history()
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

    /// Persist everything that changed since the previous checkpoint. An
    /// agent without a session keeps nothing.
    fn checkpoint(&mut self) -> Result<()> {
        let Some(session_id) = self.session_id.as_deref() else {
            return Ok(());
        };
        let checkpoint = AgentCheckpoint {
            session_id,
            changed_nodes: self.conversation.changed_nodes().collect(),
            active_path: self.conversation.path(),
            next_node_id: self.conversation.next_id(),
            changed_executions: self.journal.changed().collect(),
            next_request_id: self.next_request_id,
        };
        trace!(
            "checkpoint: {} changed node(s), {} changed execution(s)",
            checkpoint.changed_nodes.len(),
            checkpoint.changed_executions.len()
        );
        self.state_persistence
            .commit(&checkpoint, self.extensions.as_ref())?;
        self.conversation.mark_checkpointed();
        self.journal.mark_checkpointed();
        Ok(())
    }

    /// Pre-allocate the next node_id without creating a node.
    /// The returned ID is guaranteed to be used by the next `append_message` call
    /// (or `append_message_with_node_id`).
    pub fn reserve_node_id(&mut self) -> NodeId {
        self.conversation.reserve_id()
    }

    /// Adds a message to the history using a pre-allocated node_id.
    /// Use `reserve_node_id()` to obtain the ID before streaming starts,
    /// then call this after streaming completes.
    pub fn append_message_with_node_id(&mut self, message: Message, node_id: NodeId) -> Result<()> {
        self.conversation.append(message.clone(), node_id);

        for observer in &self.hooks.observers {
            observer.on_message(self.session_id.as_deref(), &message);
        }

        self.checkpoint()?;
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
        match self.run_until_complete().await {
            Err(error) if error.is::<tools_core::Cancelled>() => Ok(()),
            result => result,
        }
    }

    async fn run_until_complete(&mut self) -> Result<()> {
        let mut streaming_retry_count: u32 = 0;

        loop {
            self.cancellation.check()?;
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
                Err(e) if e.is::<tools_core::Cancelled>() => return Err(e),
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
                        tokio::select! {
                            biased;
                            _ = self.cancellation.cancelled() => return Err(tools_core::Cancelled.into()),
                            _ = tokio::time::sleep(delay) => {}
                        }
                        continue;
                    }
                    RecoveryAction::Fail => return Err(e),
                },
            };

            self.cancellation.check()?;
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

            // Persist the parser's corrected response through the same tree
            // mutation boundary used by format-on-save.
            if !truncated_response.content.is_empty()
                && truncated_response.content != llm_response.content
            {
                self.correct_last_assistant_response(&truncated_response)?;
            }

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
                        self.checkpoint()?;

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

    /// Replace the last assistant message with the parser's corrected
    /// version (e.g. text after the tool call truncated).
    fn correct_last_assistant_response(&mut self, response: &llm::LLMResponse) -> Result<()> {
        let Some(id) = self.conversation.path().last().copied() else {
            return Ok(());
        };
        if let Some(node) = self.conversation.node_mut(id)
            && node.message.role == MessageRole::Assistant
        {
            node.message.content = MessageContent::Structured(response.content.clone());
            node.message.usage = Some(response.usage.clone());
            self.checkpoint()?;
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

                    self.journal.record(ToolExecution::create_parse_error(
                        tool_id.clone(),
                        error_text.clone(),
                    ));

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
        // Inputs are already rendered, including recovery overrides. Rendering
        // executions a second time here would undo the projection for XML/caret.
        messages
            .into_iter()
            .map(|mut message| {
                if let MessageContent::Structured(blocks) = &message.content
                    && blocks
                        .iter()
                        .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
                {
                    let text: Vec<_> = blocks
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::ToolResult { content, .. } => {
                                Some(content.text_content().to_string())
                            }
                            ContentBlock::Text { text, .. } => Some(text.clone()),
                            _ => None,
                        })
                        .collect();
                    message.content = MessageContent::Text(text.join("\n\n").trim().to_string());
                }
                message
            })
            .collect()
    }

    /// Runs the iteration hooks over the rendered messages right before they
    /// are sent to the LLM (e.g. to inject system reminders).
    pub fn shape_request_messages(&mut self, mut messages: Vec<Message>) -> Vec<Message> {
        let ctx = LoopCtx {
            conversation: &mut self.conversation,
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
                        .get_tool_definitions_with_capability_excluding(
                            self.tool_capability.as_str(),
                            &self.excluded_tool_capabilities,
                        ),
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
        let cancellation = self.cancellation.clone();
        let streaming_callback: StreamingCallback = Box::new(move |chunk: &StreamingChunk| {
            cancellation.check()?;
            // Check if streaming should continue
            if !ui_for_callback.should_streaming_continue() {
                debug!("Streaming should stop - user requested cancellation");
                cancellation.cancel();
                return Err(tools_core::Cancelled.into());
            }

            let mut processor_guard = processor
                .lock()
                .map_err(|e| anyhow::anyhow!("Stream processor mutex poisoned: {e}"))?;
            processor_guard
                .process(chunk)
                .map_err(|e| anyhow::anyhow!("Failed to process streaming chunk: {e}"))
        });

        // Send message to LLM provider
        let response_result = tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => Err(tools_core::Cancelled.into()),
            result = self.llm_provider.send_message(request, Some(&streaming_callback)) => result,
        };
        let response = match response_result {
            Ok(response) => response,
            Err(e) => {
                // Check for streaming cancelled error
                if self.cancellation.is_cancelled() || e.is::<tools_core::Cancelled>() {
                    debug!("Streaming cancelled by user in LLM request {}", request_id);
                    // End LLM request with cancelled=true
                    let _ = self
                        .send_ui(AgentUiEvent::StreamingStopped {
                            request_id,
                            cancelled: true,
                            error: None,
                        })
                        .await;
                    return Err(tools_core::Cancelled.into());
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
                        .get_tool_definitions_with_capability_excluding(
                            self.tool_capability.as_str(),
                            &self.excluded_tool_capabilities,
                        ),
                ))
            } else {
                None
            },
            stop_sequences: None,
            request_id,
            session_id: self.session_id.clone().unwrap_or_default(),
        };

        let response = tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => return Err(tools_core::Cancelled.into()),
            result = self.llm_provider.send_message(request, None) => result?,
        };

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

    /// The active-path messages from the last compaction summary onwards.
    fn active_messages(&self) -> Vec<&Message> {
        let mut messages: Vec<&Message> = self.conversation.active_messages().collect();
        let start = messages
            .iter()
            .rposition(|message| message.is_compaction_summary)
            .unwrap_or(0);
        messages.drain(..start);
        messages
    }

    fn prompt_messages(&self) -> Vec<Message> {
        let path = self.conversation.path();
        let start = path
            .iter()
            .rposition(|id| {
                self.conversation
                    .nodes()
                    .get(id)
                    .is_some_and(|node| node.message.is_compaction_summary)
            })
            .unwrap_or(0);
        path[start..]
            .iter()
            .filter(|id| !self.prompt_projection.omitted_nodes.contains(id))
            .filter_map(|id| self.conversation.nodes().get(id))
            .map(|node| node.message.clone())
            .collect()
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
                // All token categories of the last request occupy the context
                // window: input and cache-write tokens are both part of the
                // prompt, cache-read tokens were replayed from the cache into
                // the prompt, and the output tokens become part of the prompt
                // on the next request.
                let used_tokens = usage
                    .input_tokens
                    .saturating_add(usage.cache_creation_input_tokens)
                    .saturating_add(usage.cache_read_input_tokens)
                    .saturating_add(usage.output_tokens);
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

    /// Shrinks only the request projection after an oversized-prompt rejection.
    /// Large results become prompt placeholders; otherwise the last exchange is
    /// omitted from the compaction request. Canonical evidence is never removed.
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
        // Keep the existing transient UI notification for a rejected output;
        // it is not a change to the persisted execution's actual outcome.
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

    /// Project the largest results from the most recent turn as small error
    /// placeholders for the next request. Original execution records survive.
    ///
    /// Returns a vec of `(tool_id, error_message)` for each replaced result,
    /// empty if nothing was replaced.  The caller is responsible for sending
    /// `UpdateToolStatus` UI events for these.
    fn replace_large_tool_results(&mut self) -> Vec<(String, String)> {
        use crate::types::PromptTooLongError;

        // Collect tool_use_ids from the last user message that contains ToolResult
        // blocks — these are the results from the most recent turn.
        let current_turn_ids: std::collections::HashSet<String> = self
            .prompt_messages()
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
        for (i, exec) in self.journal.entries().iter().enumerate() {
            if !current_turn_ids.contains(&exec.tool_request.id)
                || self
                    .prompt_projection
                    .tool_results
                    .contains_key(&exec.tool_request.id)
            {
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
            let request = &self.journal.entries()[idx].tool_request;
            let (tool_name, tool_id) = (request.name.clone(), request.id.clone());
            warn!(
                "Replacing tool result for '{}' ({}KB) with prompt-too-long error",
                tool_name,
                byte_size / 1024
            );
            let error = PromptTooLongError::new(&tool_name, byte_size);
            let error_message = error.error_message.clone();
            self.prompt_projection
                .tool_results
                .insert(tool_id.clone(), error_message.clone());
            replaced.push((tool_id, error_message));
        }

        replaced
    }

    /// Omit the last tool exchange from the prompt only, as the fallback
    /// before compaction. Canonical messages and execution evidence survive.
    fn drop_last_tool_exchange(&mut self) {
        let visible: Vec<_> = self
            .conversation
            .path()
            .iter()
            .copied()
            .filter(|id| !self.prompt_projection.omitted_nodes.contains(id))
            .collect();
        let Some(index) = visible.iter().rposition(|id| {
            self.conversation.nodes().get(id).is_some_and(|node| {
                node.message.role == MessageRole::User
                    && matches!(&node.message.content, MessageContent::Structured(blocks)
                        if blocks.iter().any(|block| matches!(block, ContentBlock::ToolResult { .. })))
            })
        }) else { return; };
        self.prompt_projection.omitted_nodes.insert(visible[index]);
        if index > 0
            && self.conversation.nodes()[&visible[index - 1]].message.role == MessageRole::Assistant
        {
            self.prompt_projection
                .omitted_nodes
                .insert(visible[index - 1]);
        }
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

    fn prompt_tool_use_ids(&self, message: &Message) -> Vec<String> {
        if message.role != MessageRole::Assistant {
            return Vec::new();
        }
        let content = match &message.content {
            MessageContent::Structured(blocks) => blocks.clone(),
            MessageContent::Text(text) => vec![ContentBlock::new_text(text.clone())],
        };
        let response = llm::LLMResponse {
            content,
            usage: llm::Usage::zero(),
            rate_limit_info: None,
        };
        self.dialect
            .extract_requests(
                &response,
                message.request_id.unwrap_or(0),
                0,
                self.registry.as_ref(),
            )
            .map(|(requests, _)| requests.into_iter().map(|request| request.id).collect())
            .unwrap_or_default()
    }

    /// Render the run-local LLM projection, never modifying conversation or
    /// tool evidence. Missing results are repaired by id in the immediate
    /// follow-up message; absent evidence means an unknown outcome, not cancel.
    pub fn render_tool_results_in_messages(&self) -> Vec<Message> {
        let mut messages = self.prompt_messages();
        let mut tracker = ResourcesTracker::new();
        let mut outputs = HashMap::new();
        // Only render executions visible in this prompt. Inactive branches and
        // omitted exchanges must not claim resources in the render tracker.
        let visible_ids: std::collections::HashSet<_> = messages
            .iter()
            .flat_map(|message| {
                let mut ids = self.prompt_tool_use_ids(message);
                if let MessageContent::Structured(blocks) = &message.content {
                    ids.extend(blocks.iter().filter_map(|block| match block {
                        ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
                        _ => None,
                    }));
                }
                ids
            })
            .collect();
        for execution in self.journal.entries().iter().rev() {
            let id = &execution.tool_request.id;
            if !visible_ids.contains(id) || outputs.contains_key(id) {
                continue;
            }
            let (content, is_error) =
                if let Some(replacement) = self.prompt_projection.tool_results.get(id) {
                    (ToolResultContent::text(replacement.clone()), true)
                } else {
                    let text = execution.result.as_render().render(&mut tracker);
                    let images = execution
                        .result
                        .render_images()
                        .into_iter()
                        .map(|image| ToolResultImage {
                            media_type: image.media_type,
                            base64_data: image.base64_data,
                        })
                        .collect();
                    (
                        ToolResultContent::with_images(text, images),
                        !execution.result.is_success(),
                    )
                };
            outputs.insert(id.clone(), (content, is_error));
        }

        // Repair only the projection, including partially recorded batches.
        let mut index = 0;
        while index < messages.len() {
            let missing: Vec<_> = if messages[index].role == MessageRole::Assistant {
                let ids = self.prompt_tool_use_ids(&messages[index]);
                ids.into_iter().filter(|id| {
                    !messages.get(index + 1).is_some_and(|next| {
                        next.role == MessageRole::User && matches!(&next.content, MessageContent::Structured(blocks)
                            if blocks.iter().any(|block| matches!(block, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == id)))
                    })
                }).map(|id| ContentBlock::ToolResult {
                    content: outputs.get(&id).map(|(content, _)| content.clone()).unwrap_or_else(|| {
                        ToolResultContent::text("Tool result is missing; execution outcome is unknown. Verify the state before retrying any side effects.")
                    }),
                    is_error: Some(outputs.get(&id).map(|(_, error)| *error).unwrap_or(true)),
                    tool_use_id: id,
                    start_time: None,
                    end_time: None,
                }).collect()
            } else {
                Vec::new()
            };
            if !missing.is_empty() {
                if let Some(next) = messages.get_mut(index + 1)
                    && next.role == MessageRole::User
                    && let MessageContent::Structured(blocks) = &mut next.content
                    && blocks
                        .iter()
                        .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
                {
                    blocks.extend(missing);
                } else {
                    messages.insert(index + 1, Message::new_user_content(missing));
                }
            }
            index += 1;
        }

        for message in &mut messages {
            match &mut message.content {
                MessageContent::Structured(blocks) => {
                    for block in blocks {
                        if let ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                            ..
                        } = block
                            && let Some((output, error)) = outputs.get(tool_use_id)
                        {
                            *content = output.clone();
                            if *error {
                                *is_error = Some(true);
                            }
                        }
                    }
                }
                MessageContent::Text(text) if message.is_compaction_summary => {
                    *text = Self::format_compaction_summary_for_prompt(text);
                }
                _ => {}
            }
        }
        messages
    }

    /// Gives the registered interceptors a chance to handle the request
    /// instead of the registry. Returns the output one of them produced.
    fn intercept_tool(
        &mut self,
        tool_request: &ToolRequest,
    ) -> Option<Result<Box<dyn tools_core::AnyOutput>>> {
        let mut ctx = LoopCtx {
            conversation: &mut self.conversation,
            session_id: self.session_id.as_deref(),
            registry: self.registry.as_ref(),
            extensions: self.extensions.as_mut(),
        };
        self.hooks
            .interceptors
            .iter()
            .find_map(|interceptor| interceptor.try_intercept(tool_request, &mut ctx))
    }

    /// Notifies the registered interceptors that a tool executed successfully.
    fn after_tool_success(&mut self, tool_request: &ToolRequest) {
        let mut ctx = LoopCtx {
            conversation: &mut self.conversation,
            session_id: self.session_id.as_deref(),
            registry: self.registry.as_ref(),
            extensions: self.extensions.as_mut(),
        };
        for interceptor in &self.hooks.interceptors {
            interceptor.after_tool_success(tool_request, &mut ctx);
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

    /// Persist formatted inputs through the conversation mutation boundary.
    fn update_message_history_with_formatted_tool(
        &mut self,
        updated_request: &ToolRequest,
    ) -> Result<()> {
        let dialect = self.dialect.clone();
        let registry = self.registry.clone();
        let Some(node) = self.conversation.last_assistant_node_mut() else {
            return Ok(());
        };
        let message = &mut node.message;
        let request_id = message.request_id.unwrap_or(0);
        let updated = match &mut message.content {
            MessageContent::Structured(blocks) => {
                let native_call = blocks.iter_mut().find_map(|block| match block {
                    ContentBlock::ToolUse {
                        id, name, input, ..
                    } if id == &updated_request.id && name == &updated_request.name => Some(input),
                    _ => None,
                });
                match native_call {
                    Some(input) => {
                        *input = updated_request.input.clone();
                        true
                    }
                    None if !dialect.uses_native_tools() => Self::update_tool_call_in_text_blocks(
                        blocks,
                        updated_request,
                        request_id,
                        dialect.as_ref(),
                        registry.as_ref(),
                    ),
                    None => false,
                }
            }
            MessageContent::Text(text) => {
                match Self::update_tool_call_in_text_static(
                    text,
                    updated_request,
                    dialect.as_ref(),
                    registry.as_ref(),
                ) {
                    Ok(replacement) => {
                        *text = replacement;
                        true
                    }
                    Err(_) => false,
                }
            }
        };
        if updated {
            self.checkpoint()?;
        } else {
            warn!("Could not find tool call {} to update", updated_request.id);
        }
        Ok(())
    }

    fn update_tool_call_in_text_blocks(
        blocks: &mut [ContentBlock],
        request: &ToolRequest,
        request_id: u64,
        dialect: &dyn ToolDialect,
        registry: &ToolRegistry,
    ) -> bool {
        // XML/caret offsets are local to the Text block that was parsed.
        // Reparse that block to find the id and current offsets: an earlier
        // formatted call may have changed its length. Never rewrite preambles
        // or thinking blocks merely because their offsets happen to fit.
        for block in blocks {
            if let ContentBlock::Text { text, .. } = block {
                let response = llm::LLMResponse {
                    content: vec![ContentBlock::new_text(text.clone())],
                    usage: llm::Usage::zero(),
                    rate_limit_info: None,
                };
                if let Ok((requests, _)) =
                    dialect.extract_requests(&response, request_id, 0, registry)
                    && let Some(current) = requests
                        .iter()
                        .find(|current| current.id == request.id && current.name == request.name)
                {
                    let mut corrected = request.clone();
                    corrected.start_offset = current.start_offset;
                    corrected.end_offset = current.end_offset;
                    if let Ok(replacement) =
                        Self::update_tool_call_in_text_static(text, &corrected, dialect, registry)
                    {
                        *text = replacement;
                        return true;
                    }
                }
            }
        }
        false
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
