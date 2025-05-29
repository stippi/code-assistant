use crate::agent::tool_description_generator::generate_tool_documentation;
use crate::agent::types::{ToolExecution, ToolRequest};
use crate::config::ProjectManager;
use crate::persistence::StatePersistence;
use crate::tools::core::{ResourcesTracker, ToolContext, ToolRegistry, ToolScope};
use crate::tools::{parse_tool_xml, TOOL_TAG_PREFIX};
use crate::types::*;
use crate::ui::{streaming::create_stream_processor, UIMessage, UserInterface};
use crate::utils::CommandExecutor;
use anyhow::Result;
use llm::{
    ContentBlock, LLMProvider, LLMRequest, Message, MessageContent, MessageRole, StreamingCallback,
    StreamingChunk,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use tracing::debug;

use super::ToolMode;

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
    tool_mode: ToolMode,
    project_manager: Box<dyn ProjectManager>,
    command_executor: Box<dyn CommandExecutor>,
    ui: Arc<Box<dyn UserInterface>>,
    state_persistence: Box<dyn StatePersistence>,
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
                    format!("Tool parameter error: {}. Please try again.", msg)
                }
            }
        } else {
            // Generic fallback for other error types
            format!("Error in tool request: {}", error)
        }
    }

    pub fn new(
        llm_provider: Box<dyn LLMProvider>,
        tool_mode: ToolMode,
        project_manager: Box<dyn ProjectManager>,
        command_executor: Box<dyn CommandExecutor>,
        ui: Box<dyn UserInterface>,
        state_persistence: Box<dyn StatePersistence>,
        init_path: Option<PathBuf>,
    ) -> Self {
        Self {
            working_memory: WorkingMemory::default(),
            llm_provider,
            tool_mode,
            project_manager,
            ui: Arc::new(ui),
            command_executor,
            state_persistence,
            message_history: Vec::new(),
            init_path,
            initial_project: None,
            tool_executions: Vec::new(),
            cached_system_message: OnceLock::new(),
        }
    }

    /// Save the current state (message history)
    fn save_state(&mut self) -> Result<()> {
        self.state_persistence.save_state(
            self.working_memory.current_task.clone(),
            self.message_history.clone(),
        )?;
        Ok(())
    }

    /// Adds a message to the history and saves the state
    fn append_message(&mut self, message: Message) -> Result<()> {
        self.message_history.push(message);
        self.save_state()?;
        Ok(())
    }

    pub async fn get_input_from_ui(&self) -> Result<String> {
        self.ui.get_input().await.map_err(|e| e.into())
    }

    /// Handles the interaction with the LLM to get the next assistant message.
    /// Appends the assistant's message to the history.
    async fn obtain_llm_response(&mut self, messages: Vec<Message>) -> Result<llm::LLMResponse> {
        let llm_response = self.get_next_assistant_message(messages).await?;
        self.append_message(Message {
            role: MessageRole::Assistant,
            content: MessageContent::Structured(llm_response.content.clone()),
        })?;
        Ok(llm_response)
    }

    /// Parses tool requests from the LLM response.
    /// Returns a tuple of tool requests and a LoopFlow indicating what action should be taken next.
    /// - If parsing succeeds and requests are empty: returns (empty vec, GetUserInput)
    /// - If parsing succeeds and requests exist: returns (requests, Continue)
    /// - If parsing fails: adds an error message to history and returns (empty vec, Continue)
    async fn extract_tool_requests_from_response(
        &mut self,
        llm_response: &llm::LLMResponse,
        request_counter: u64,
    ) -> Result<(Vec<ToolRequest>, LoopFlow)> {
        match parse_llm_response(llm_response, request_counter) {
            Ok(requests) => {
                if requests.is_empty() {
                    Ok((requests, LoopFlow::GetUserInput))
                } else {
                    Ok((requests, LoopFlow::Continue))
                }
            }
            Err(e) => {
                let error_text = Self::format_error_for_user(&e);
                let error_msg = Message {
                    role: MessageRole::User,
                    content: MessageContent::Text(error_text),
                };
                self.append_message(error_msg)?;
                Ok((Vec::new(), LoopFlow::Continue)) // Continue without user input on parsing errors
            }
        }
    }

    /// Handles the case where no tool requests are made by the LLM.
    /// Prompts the user for input and adds it to the message history.
    async fn solicit_user_input(&mut self) -> Result<()> {
        let user_input = self.get_input_from_ui().await?;
        self.ui
            .display(UIMessage::UserInput(user_input.clone()))
            .await?;
        let user_msg = Message {
            role: MessageRole::User,
            content: MessageContent::Text(user_input.clone()),
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
                self.state_persistence.cleanup()?;
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
            };
            self.append_message(result_message)?;
        }
        Ok(LoopFlow::Continue)
    }

    async fn run_agent_loop(&mut self) -> Result<()> {
        let mut request_counter: u64 = 0;

        loop {
            let messages = self.prepare_messages();
            if self.message_history.is_empty() {
                // This ensures that on the very first run, the initial task (user message)
                // is correctly set up in message_history if `prepare_messages` returns it.
                // `prepare_messages` is designed to return the current task if history is empty.
                self.message_history = messages.clone();
            }
            request_counter += 1;

            // 1. Obtain LLM response (includes adding assistant message to history)
            let llm_response = match self.obtain_llm_response(messages).await {
                Ok(response) => response,
                Err(e) => {
                    // Log critical error and break loop
                    tracing::error!("Critical error obtaining LLM response: {}", e);
                    return Err(e);
                }
            };

            // 2. Extract tool requests from LLM response and determine the next flow action
            let (tool_requests, flow) = self
                .extract_tool_requests_from_response(&llm_response, request_counter)
                .await?;

            // 3. Act based on the flow instruction
            match flow {
                LoopFlow::GetUserInput => {
                    // LLM explicitly requested user input (no tools requested)
                    self.solicit_user_input().await?;
                }
                LoopFlow::Continue => {
                    if !tool_requests.is_empty() {
                        // Tools were requested, manage their execution
                        match self.manage_tool_execution(&tool_requests).await? {
                            LoopFlow::Continue => { /* Continue to the next iteration */ }
                            LoopFlow::GetUserInput => {
                                // Handle case where tool execution results in needing user input
                                self.solicit_user_input().await?;
                            }
                            LoopFlow::Break => {
                                // Task completed (e.g., via complete_task tool)
                                return Ok(());
                            }
                        }
                    }
                    // If tool_requests is empty here, it was a parsing error
                    // and we continue to the next iteration without user input
                }
                LoopFlow::Break => {
                    // Immediately break the loop
                    return Ok(());
                }
            }

            // Notify UI of working memory change at the end of each cycle
            let _ = self.ui.update_memory(&self.working_memory).await;
        }
    }

    fn init_working_memory(&mut self, task: String) -> Result<()> {
        self.working_memory.current_task = task.clone();

        // Initialize empty structures for multi-project support
        self.working_memory.file_trees = HashMap::new();
        self.working_memory.available_projects = Vec::new();

        // Reset the initial project
        self.initial_project = None;

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

    /// Start a new agent task
    pub async fn start_with_task(&mut self, task: String) -> Result<()> {
        debug!("Starting agent with task: {}", task);

        self.init_working_memory(task.clone())?;

        self.message_history.clear();
        self.ui.display(UIMessage::UserInput(task.clone())).await?;

        // Create the initial user message
        let user_msg = Message {
            role: MessageRole::User,
            content: MessageContent::Text(task.clone()),
        };
        self.append_message(user_msg)?;

        // Notify UI of initial working memory
        let _ = self.ui.update_memory(&self.working_memory).await;

        self.run_agent_loop().await
    }

    /// Continue from a saved state
    pub async fn start_from_state(&mut self) -> Result<()> {
        if let Some(state) = self.state_persistence.load_state()? {
            debug!("Continuing task: {}", state.task);

            // Initialize working memory
            self.init_working_memory(state.task.clone())?;

            // Restore message history
            self.message_history = state.messages;
            debug!("Restored {} previous messages", self.message_history.len());

            // Load current state of files into memory - will create file trees as needed
            self.load_current_files_to_memory().await?;

            self.ui
                .display(UIMessage::Action(format!(
                    "Continuing task: {}, loaded {} previous messages",
                    state.task,
                    self.message_history.len()
                )))
                .await?;

            // Notify UI of loaded working memory
            let _ = self.ui.update_memory(&self.working_memory).await;

            self.run_agent_loop().await
        } else {
            anyhow::bail!("No saved state found")
        }
    }

    /// Initialize file trees for available projects
    /// This is a simplified version that doesn't rely on action_history
    async fn load_current_files_to_memory(&mut self) -> Result<()> {
        // In the new version, we don't need to build file trees from action history
        // Instead, we'll just initialize file trees for the available projects

        // If there's an initial project, make sure it's in the list
        if let Some(project_name) = &self.initial_project {
            // Create file tree for the project
            match self.project_manager.get_explorer_for_project(project_name) {
                Ok(mut explorer) => {
                    match explorer.create_initial_tree(2) {
                        Ok(tree) => {
                            self.working_memory
                                .file_trees
                                .insert(project_name.clone(), tree);

                            // Add to available projects if not already there
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
                        Err(e) => {
                            debug!(
                                "Error creating file tree for project {}: {}",
                                project_name, e
                            );
                        }
                    }
                }
                Err(e) => {
                    debug!("Error getting explorer for project {}: {}", project_name, e);
                }
            }
        }

        Ok(())
    }

    /// Get the appropriate system prompt based on tool mode
    fn get_system_prompt(&self) -> String {
        // Check if we already have a cached system message
        if let Some(cached) = self.cached_system_message.get() {
            return cached.clone();
        }

        // Generate the system message
        let mut system_message = match self.tool_mode {
            ToolMode::Native => SYSTEM_MESSAGE.to_string(),
            ToolMode::Xml => {
                // For XML tool mode, get the base template and replace the {{tools}} placeholder
                let mut base = SYSTEM_MESSAGE_TOOLS.to_string();

                // Only generate tools documentation for XML mode
                let tools_doc = generate_tool_documentation(ToolScope::Agent);

                // Replace the {{tools}} placeholder with the generated documentation
                base = base.replace("{{tools}}", &tools_doc);

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
                                ContentBlock::ToolResult { tool_use_id, .. } => {
                                    // Get the dynamically rendered content for this tool result
                                    if let Some(rendered_output) = tool_outputs.get(tool_use_id) {
                                        // Add the rendered tool output
                                        text_content.push_str(rendered_output);
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
                        }
                    }
                    // For non-structured content, keep as is
                    _ => msg,
                }
            })
            .collect()
    }

    /// Gets the next assistant message from the LLM provider.
    async fn get_next_assistant_message(&self, messages: Vec<Message>) -> Result<llm::LLMResponse> {
        // Inform UI that a new LLM request is starting
        let request_id = self.ui.begin_llm_request().await?;
        debug!("Starting LLM request with ID: {}", request_id);

        // Convert messages based on tool mode
        let converted_messages = match self.tool_mode {
            ToolMode::Native => messages, // No conversion needed
            ToolMode::Xml => self.convert_tool_results_to_text(messages), // Convert ToolResults to Text
        };

        // Create the LLM request with appropriate tools
        let request = LLMRequest {
            messages: converted_messages,
            system_prompt: self.get_system_prompt(),
            tools: match self.tool_mode {
                ToolMode::Native => {
                    Some(crate::tools::AnnotatedToolDefinition::to_tool_definitions(
                        ToolRegistry::global().get_tool_definitions_for_scope(ToolScope::Agent),
                    ))
                }
                ToolMode::Xml => None,
            },
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

        // Create a StreamProcessor and use it to process streaming chunks
        let ui = Arc::clone(&self.ui);
        let processor = Arc::new(Mutex::new(create_stream_processor(
            self.tool_mode,
            ui.clone(),
        )));

        let streaming_callback: StreamingCallback = Box::new(move |chunk: &StreamingChunk| {
            // Check if streaming should continue
            if !ui.should_streaming_continue() {
                debug!("Streaming should stop - user requested cancellation");
                return Err(anyhow::anyhow!("Streaming cancelled by user"));
            }

            let mut processor_guard = processor.lock().unwrap();
            processor_guard
                .process(chunk)
                .map_err(|e| anyhow::anyhow!("Failed to process streaming chunk: {}", e))
        });

        // Send message to LLM provider
        let response = self
            .llm_provider
            .send_message(request, Some(&streaming_callback))
            .await?;

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

        println!(
            "\n==== Token usage: Input: {}, Output: {}, Cache: Created: {}, Read: {} ====\n",
            response.usage.input_tokens,
            response.usage.output_tokens,
            response.usage.cache_creation_input_tokens,
            response.usage.cache_read_input_tokens
        );

        // Inform UI that the LLM request has completed
        let _ = self.ui.end_llm_request(request_id).await;
        debug!("Completed LLM request with ID: {}", request_id);

        Ok(response)
    }

    /// Prepare messages for LLM request, dynamically rendering tool outputs
    fn prepare_messages(&self) -> Vec<Message> {
        if self.message_history.is_empty() {
            // Initial message with just the task
            return vec![Message {
                role: MessageRole::User,
                content: MessageContent::Text(self.working_memory.current_task.clone()),
            }];
        }

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
            .update_tool_status(&tool_request.id, crate::ui::ToolStatus::Running, None, None)
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
            .update_tool_status(&tool_request.id, status, Some(short_output), Some(output))
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

pub(crate) fn parse_llm_response(
    response: &llm::LLMResponse,
    request_id: u64,
) -> Result<Vec<ToolRequest>> {
    let mut tool_requests = Vec::new();
    let mut reasoning = String::new();

    for block in &response.content {
        if let ContentBlock::Text { text } = block {
            let mut current_pos = 0;

            while let Some(tool_start) = text[current_pos..].find(&format!("<{}", TOOL_TAG_PREFIX))
            {
                let abs_start = current_pos + tool_start;

                // Add text before tool to reasoning (kept for potential future use)
                reasoning.push_str(text[current_pos..abs_start].trim());
                if !reasoning.is_empty() {
                    reasoning.push('\n');
                }

                // Find the root tag name
                let tag_name = text[abs_start..]
                    .split('>')
                    .next()
                    .and_then(|s| s.strip_prefix('<'))
                    .ok_or_else(|| anyhow::anyhow!("Invalid XML: missing tag name"))?;

                // Only process tags with our tool prefix
                if let Some(tool_name) = tag_name.strip_prefix(TOOL_TAG_PREFIX) {
                    // Find closing tag for the root element
                    let closing_tag = format!("</{}{}>", TOOL_TAG_PREFIX, tool_name);
                    if let Some(rel_end) = text[abs_start..].find(&closing_tag) {
                        let abs_end = abs_start + rel_end + closing_tag.len();
                        let tool_content = &text[abs_start..abs_end];
                        debug!("Found tool content:\n{}", tool_content);

                        // Parse the tool XML to get tool name and parameters
                        let (tool_name, tool_params) = parse_tool_xml(tool_content)?;

                        // Check if the tool exists in the registry before creating a ToolRequest
                        if ToolRegistry::global().get(&tool_name).is_none() {
                            return Err(ToolError::UnknownTool(tool_name).into());
                        }

                        // Generate a unique tool ID that matches the one generated by the UI
                        // The UI increments its counter before generating an ID, so we add 1
                        let tool_id = format!("tool-{}-{}", request_id, tool_requests.len() + 1);

                        // Create a ToolRequest
                        let tool_request = ToolRequest {
                            id: tool_id,
                            name: tool_name,
                            input: tool_params,
                        };

                        tool_requests.push(tool_request);
                        current_pos = abs_end;
                        continue;
                    }
                }

                // If we get here, either the tag didn't have our prefix or we didn't find the closing tag
                // In both cases, treat it as regular text
                reasoning.push_str(&text[abs_start..abs_start + 1]);
                current_pos = abs_start + 1;
            }

            // Add any remaining text to reasoning
            if current_pos < text.len() {
                reasoning.push_str(text[current_pos..].trim());
            }
        }

        if let ContentBlock::ToolUse { id, name, input } = block {
            // For ToolUse blocks, create ToolRequest directly
            let tool_request = ToolRequest {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            };
            tool_requests.push(tool_request);
            reasoning = String::new();
        }
    }

    Ok(tool_requests)
}
