use crate::agent::persistence::AgentStatePersistence;
use crate::agent::types::ToolExecution;
use crate::config::ProjectManager;
use crate::persistence::ChatMetadata;
use crate::tools::core::{ResourcesTracker, ToolContext, ToolRegistry, ToolScope};
use crate::tools::{ParserRegistry, ToolRequest};
use crate::types::*;
use crate::ui::{UiEvent, UserInterface};
use crate::utils::CommandExecutor;
use anyhow::Result;
use llm::{
    ContentBlock, LLMProvider, LLMRequest, Message, MessageContent, MessageRole, StreamingCallback,
    StreamingChunk,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;
use tracing::{debug, trace, warn};

use super::ToolSyntax;

// System messages
const SYSTEM_MESSAGE: &str = include_str!("../../resources/system_message.md");
const SYSTEM_MESSAGE_TOOLS: &str = include_str!("../../resources/system_message_tools.md");

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
    llm_provider: Box<dyn LLMProvider>,
    tool_syntax: ToolSyntax,
    project_manager: Box<dyn ProjectManager>,
    command_executor: Box<dyn CommandExecutor>,
    ui: Arc<Box<dyn UserInterface>>,
    state_persistence: Box<dyn AgentStatePersistence>,
    // Store all messages exchanged
    message_history: Vec<Message>,
    // Path provided during agent initialization
    init_path: Option<PathBuf>,
    // Name of the initial project (if any)
    initial_project: Option<String>,
    // Store the history of tool executions
    tool_executions: Vec<crate::agent::types::ToolExecution>,
    // Cached system message
    cached_system_message: OnceLock<String>,
    // Counter for generating unique request IDs
    next_request_id: u64,
    // Session ID for this agent instance
    session_id: Option<String>,
}

impl Agent {
    /// Formats an error, particularly ToolErrors, into a user-friendly string.
    fn format_error_for_user(error: &anyhow::Error) -> String {
        if let Some(tool_error) = error.downcast_ref::<ToolError>() {
            match tool_error {
                ToolError::UnknownTool(t) => {
                    format!("Unknown tool '{}'. Please use only available tools.", t)
                }
                ToolError::ParseError(msg) => {
                    format!("Tool error: {}. Please try again.", msg)
                }
            }
        } else {
            // Generic fallback for other error types
            format!("Error in tool request: {}", error)
        }
    }

    pub fn new(
        llm_provider: Box<dyn LLMProvider>,
        tool_syntax: ToolSyntax,
        project_manager: Box<dyn ProjectManager>,
        command_executor: Box<dyn CommandExecutor>,
        ui: Arc<Box<dyn UserInterface>>,
        state_persistence: Box<dyn AgentStatePersistence>,
        init_path: Option<PathBuf>,
    ) -> Self {
        Self {
            working_memory: WorkingMemory::default(),
            llm_provider,
            tool_syntax,
            project_manager,
            ui,
            command_executor,
            state_persistence,
            message_history: Vec::new(),
            init_path,
            initial_project: None,
            tool_executions: Vec::new(),
            cached_system_message: OnceLock::new(),
            next_request_id: 1, // Start from 1
            session_id: None,
        }
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
                name: format!("Session {}", &session_id[..8]), // Default name, will be overridden by persistence
                created_at: SystemTime::now(),                 // Will be overridden by persistence
                updated_at: SystemTime::now(),
                message_count: self.message_history.len(),
                total_usage,
                last_usage,
                tokens_limit,
            }
        })
    }

    /// Save the current state (message history and tool executions)
    fn save_state(&mut self) -> Result<()> {
        trace!(
            "saving {} messages to persistence",
            self.message_history.len()
        );
        self.state_persistence.save_agent_state(
            self.message_history.clone(),
            self.tool_executions.clone(),
            self.working_memory.clone(),
            self.init_path.clone(),
            self.initial_project.clone(),
            self.next_request_id,
        )?;

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

    /// Run the agent loop until task completion
    /// Get user input whenever there is no tool use by the LLM
    pub async fn run_agent_loop(&mut self) -> Result<()> {
        loop {
            // Run a single iteration and check if user input is needed
            let needs_user_input = self.run_single_iteration_internal().await?;

            if needs_user_input {
                // LLM explicitly requested user input (no tools requested)
                self.solicit_user_input().await?;
            } else {
                // Task completed or loop should break
                return Ok(());
            }
        }
    }

    /// Run a single iteration of the agent loop without waiting for user input
    /// This is used in the new on-demand agent architecture
    pub async fn run_single_iteration(&mut self) -> Result<()> {
        let _needs_user_input = self.run_single_iteration_internal().await?;
        // In on-demand mode, we don't handle user input - that's done externally
        // Just ignore the needs_user_input flag and return
        Ok(())
    }

    /// Internal helper for running a single iteration of the agent calling tools in a loop
    /// Returns whether user input is needed before the next iteration
    async fn run_single_iteration_internal(&mut self) -> Result<bool> {
        loop {
            let messages = self.render_tool_results_in_messages();

            // 1. Get LLM response (without adding to history yet)
            let (llm_response, request_id) = self.get_next_assistant_message(messages).await?;

            // 2. Add original LLM response to message history if it has content
            if !llm_response.content.is_empty() {
                self.append_message(Message {
                    role: MessageRole::Assistant,
                    content: MessageContent::Structured(llm_response.content.clone()),
                    request_id: Some(request_id),
                    usage: Some(llm_response.usage.clone()),
                })?;
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
                    return Ok(true); // User input needed
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
                                return Ok(true); // User input needed
                            }
                            LoopFlow::Break => {
                                // Task completed (e.g., via complete_task tool)
                                debug!("Task completed");
                                return Ok(false); // No user input needed, task complete
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
                    return Ok(false); // No user input needed, task complete
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
        warn!(
            "loaded {} messages from session",
            self.message_history.len()
        );
        self.tool_executions = session_state.tool_executions;
        self.working_memory = session_state.working_memory;
        self.init_path = session_state.init_path;
        self.initial_project = session_state.initial_project;

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

        Ok(())
    }

    pub fn init_working_memory(&mut self) -> Result<()> {
        // Initialize empty structures for multi-project support
        self.working_memory.file_trees = HashMap::new();
        self.working_memory.available_projects = Vec::new();

        // Reset the initial project
        self.initial_project = None;

        self.init_working_memory_projects()
    }

    fn init_working_memory_projects(&mut self) -> Result<()> {
        // If a path was provided in args, add it as a temporary project
        if let Some(path) = &self.init_path {
            // Add as temporary project and get its name
            let project_name = self.project_manager.add_temporary_project(path.clone())?;

            // Store the name of the initial project
            self.initial_project = Some(project_name.clone());

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
        let parser = ParserRegistry::get(self.tool_syntax);
        match parser.extract_requests(llm_response, request_counter, 0) {
            Ok((requests, truncated_response)) => {
                if requests.is_empty() {
                    Ok((requests, LoopFlow::GetUserInput, truncated_response))
                } else {
                    Ok((requests, LoopFlow::Continue, truncated_response))
                }
            }
            Err(e) => {
                let error_text = Self::format_error_for_user(&e);

                let error_msg = match self.tool_syntax {
                    ToolSyntax::Xml => {
                        // For XML mode, create structured tool-result message like regular tool results
                        // Generate normal tool ID for consistency with UI expectations
                        let tool_id = format!("tool-{}-0", request_counter);

                        // Create and store a ToolExecution for the parse error
                        let tool_execution =
                            ToolExecution::create_parse_error(tool_id.clone(), error_text.clone());
                        self.tool_executions.push(tool_execution);

                        Message {
                            role: MessageRole::User,
                            content: MessageContent::Structured(vec![ContentBlock::ToolResult {
                                tool_use_id: tool_id,
                                content: error_text,
                                is_error: Some(true),
                            }]),
                            request_id: None,
                            usage: None,
                        }
                    }
                    ToolSyntax::Native => {
                        // For native mode, keep text message since parsing errors occur before
                        // we have any LLM-provided tool IDs to reference
                        Message {
                            role: MessageRole::User,
                            content: MessageContent::Text(error_text),
                            request_id: None,
                            usage: None,
                        }
                    }
                    ToolSyntax::Caret => {
                        // For caret mode, use text message like native mode
                        Message {
                            role: MessageRole::User,
                            content: MessageContent::Text(error_text),
                            request_id: None,
                            usage: None,
                        }
                    }
                };

                self.append_message(error_msg)?;
                // Return original response for error cases
                Ok((Vec::new(), LoopFlow::Continue, llm_response.clone())) // Continue without user input on parsing errors
            }
        }
    }

    /// Handles the case where no tool requests are made by the LLM.
    /// Prompts the user for input and adds it to the message history.
    async fn solicit_user_input(&mut self) -> Result<()> {
        let user_input = self.ui.get_input().await?;
        self.ui
            .send_event(UiEvent::DisplayUserInput {
                content: user_input.clone(),
            })
            .await?;
        let user_msg = Message {
            role: MessageRole::User,
            content: MessageContent::Text(user_input.clone()),
            request_id: None,
            usage: None,
        };
        self.append_message(user_msg)?;
        Ok(())
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

            let result_block = match self.execute_tool(tool_request).await {
                Ok(success) => ContentBlock::ToolResult {
                    tool_use_id: tool_request.id.clone(),
                    content: String::new(), // Will be filled dynamically in prepare_messages
                    is_error: if success { None } else { Some(true) },
                },
                Err(e) => {
                    let error_text = Self::format_error_for_user(&e);
                    ContentBlock::ToolResult {
                        tool_use_id: tool_request.id.clone(),
                        content: error_text,
                        is_error: Some(true),
                    }
                }
            };
            content_blocks.push(result_block);
        }

        // Only add message if there were actual tool executions (not just complete_task)
        if !content_blocks.is_empty() {
            let result_message = Message {
                role: MessageRole::User,
                content: MessageContent::Structured(content_blocks),
                request_id: None,
                usage: None,
            };
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
            })
            .await?;

        // Create the initial user message
        let user_msg = Message {
            role: MessageRole::User,
            content: MessageContent::Text(task.clone()),
            request_id: None,
            usage: None,
        };
        self.append_message(user_msg)?;

        // Notify UI of initial working memory
        let _ = self
            .ui
            .send_event(UiEvent::UpdateMemory {
                memory: self.working_memory.clone(),
            })
            .await;

        self.run_agent_loop().await
    }

    /// Get the appropriate system prompt based on tool mode
    fn get_system_prompt(&self) -> String {
        // Check if we already have a cached system message
        if let Some(cached) = self.cached_system_message.get() {
            return cached.clone();
        }

        // Generate the system message
        let mut system_message = match self.tool_syntax {
            ToolSyntax::Native => SYSTEM_MESSAGE.to_string(),
            _ => {
                // For XML and Caret modes, get the base template and replace the {{tools}} placeholder
                let mut base = SYSTEM_MESSAGE_TOOLS.to_string();

                // Get parser and generate syntax-specific tools documentation
                let parser = ParserRegistry::get(self.tool_syntax);
                if let Some(tools_doc) = parser.generate_tool_documentation(ToolScope::Agent) {
                    // Replace the {{tools}} placeholder with the generated documentation
                    base = base.replace("{{tools}}", &tools_doc);
                } else {
                    // Fallback to empty tools section if no documentation needed
                    base = base.replace("{{tools}}", "");
                }

                base
            }
        };

        // Add project information
        let mut project_info = String::new();

        // Add information about the initial project if available
        if let Some(project) = &self.initial_project {
            project_info.push_str("\n\n# Project Information\n\n");
            project_info.push_str(&format!("## Initial Project: {}\n\n", project));

            // Add file tree for the initial project if available
            if let Some(tree) = self.working_memory.file_trees.get(project) {
                project_info.push_str("### File Structure:\n");
                project_info.push_str(&format!("```\n{}\n```\n\n", tree.to_string()));
            }
        }

        // Add information about available projects
        if !self.working_memory.available_projects.is_empty() {
            project_info.push_str("## Available Projects:\n");
            for project in &self.working_memory.available_projects {
                project_info.push_str(&format!("- {}\n", project));
            }
        }

        // Append project information to base prompt if available
        if !project_info.is_empty() {
            system_message = format!("{}\n{}", system_message, project_info);
        }

        // Cache the system message
        let _ = self.cached_system_message.set(system_message.clone());

        system_message
    }

    /// Invalidate the cached system message to force regeneration
    #[allow(dead_code)]
    pub fn invalidate_system_message_cache(&mut self) {
        self.cached_system_message = OnceLock::new();
    }

    /// Convert ToolResult blocks to Text blocks for XML mode
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
                                ContentBlock::ToolResult {
                                    tool_use_id,
                                    content,
                                    ..
                                } => {
                                    // Get the dynamically rendered content for this tool result
                                    if let Some(rendered_output) = tool_outputs.get(tool_use_id) {
                                        // Add the rendered tool output from actual tool execution
                                        text_content.push_str(rendered_output);
                                        text_content.push_str("\n\n");
                                    } else if tool_use_id.starts_with("failed-tool-") {
                                        // For failed tool error messages, use the content directly
                                        text_content.push_str(content);
                                        text_content.push_str("\n\n");
                                    }
                                }
                                ContentBlock::Text { text } => {
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
                        }
                    }
                    // For non-structured content, keep as is
                    _ => msg,
                }
            })
            .collect()
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

        // Convert messages based on tool syntax
        // Native mode keeps ToolUse blocks, all other syntaxes convert to text
        let converted_messages = match self.tool_syntax {
            ToolSyntax::Native => messages, // No conversion needed
            _ => self.convert_tool_results_to_text(messages), // Convert ToolResults to Text
        };

        // Create the LLM request with appropriate tools
        let request = LLMRequest {
            messages: converted_messages,
            system_prompt: self.get_system_prompt(),
            tools: match self.tool_syntax {
                ToolSyntax::Native => {
                    Some(crate::tools::AnnotatedToolDefinition::to_tool_definitions(
                        ToolRegistry::global().get_tool_definitions_for_scope(ToolScope::Agent),
                    ))
                }
                ToolSyntax::Xml => None,
                ToolSyntax::Caret => None,
            },
            stop_sequences: None,
        };

        // Log messages for debugging
        for (i, message) in request.messages.iter().enumerate() {
            debug!("Message {}:", i);
            // Using the Display trait implementation for Message
            let formatted_message = format!("{}", message);
            // Add indentation to the message output
            let indented = formatted_message
                .lines()
                .map(|line| format!("  {}", line))
                .collect::<Vec<String>>()
                .join("\n");
            debug!("{}", indented);
        }

        // Create a StreamProcessor with the UI and request ID
        let parser = ParserRegistry::get(self.tool_syntax);
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
                .map_err(|e| anyhow::anyhow!("Failed to process streaming chunk: {}", e))
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
                    })
                    .await;
                return Err(e);
            }
        };

        // Print response for debugging
        debug!("Raw LLM response:");
        for block in &response.content {
            match block {
                ContentBlock::Text { text } => {
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
            })
            .await;
        debug!("Completed LLM request with ID: {}", request_id);

        Ok((response, request_id))
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
        for msg in &self.message_history {
            match &msg.content {
                MessageContent::Structured(blocks) => {
                    // Look for ToolResult blocks
                    let mut new_blocks = Vec::new();
                    let mut need_update = false;

                    for block in blocks {
                        if let ContentBlock::ToolResult {
                            tool_use_id,
                            is_error,
                            ..
                        } = block
                        {
                            // If we have an execution result for this tool use, use it
                            if let Some(output) = tool_outputs.get(tool_use_id) {
                                // Create a new ToolResult with updated content
                                new_blocks.push(ContentBlock::ToolResult {
                                    tool_use_id: tool_use_id.clone(),
                                    content: output.clone(),
                                    is_error: *is_error,
                                });
                                need_update = true;
                            } else {
                                // Keep the original block
                                new_blocks.push(block.clone());
                            }
                        } else {
                            // Keep non-ToolResult blocks as is
                            new_blocks.push(block.clone());
                        }
                    }

                    if need_update {
                        // Create a new message with updated blocks
                        let new_msg = Message {
                            role: msg.role.clone(),
                            content: MessageContent::Structured(new_blocks),
                            request_id: msg.request_id,
                            usage: msg.usage.clone(),
                        };
                        messages.push(new_msg);
                    } else {
                        // No changes needed, use original message
                        messages.push(msg.clone());
                    }
                }
                _ => {
                    // For non-tool messages, just copy them as is
                    messages.push(msg.clone());
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

        // Update status to Running before execution
        self.ui
            .send_event(UiEvent::UpdateToolStatus {
                tool_id: tool_request.id.clone(),
                status: crate::ui::ToolStatus::Running,
                message: None,
                output: None,
            })
            .await?;

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
        };

        // Execute the tool - could fail with ParseError or other errors
        let result = tool
            .invoke(&mut context, tool_request.input.clone())
            .await?;

        // Get success status from result
        let success = result.is_success();

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

        // Update tool status with result
        self.ui
            .send_event(UiEvent::UpdateToolStatus {
                tool_id: tool_request.id.clone(),
                status,
                message: Some(short_output),
                output: Some(output),
            })
            .await?;

        // Create and store the ToolExecution record
        let tool_execution = ToolExecution {
            tool_request: tool_request.clone(),
            result,
        };

        // Store the execution record
        self.tool_executions.push(tool_execution);

        Ok(success)
    }
}

/// Parse tool requests from LLM response and return both requests and truncated response after first tool
/// This is a wrapper that defaults to XML parsing for backward compatibility
pub fn parse_and_truncate_llm_response(
    response: &llm::LLMResponse,
    request_id: u64,
) -> Result<(Vec<ToolRequest>, llm::LLMResponse)> {
    // Default to XML parser for backward compatibility with existing tests
    let parser = ParserRegistry::get(ToolSyntax::Xml);
    parser.extract_requests(response, request_id, 0)
}
