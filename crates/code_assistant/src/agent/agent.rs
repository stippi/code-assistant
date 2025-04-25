use crate::agent::types::{ToolExecution, ToolRequest};
use crate::config::ProjectManager;
use crate::persistence::StatePersistence;
use crate::tools::core::{ToolContext, ToolRegistry};
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
use std::sync::{Arc, Mutex};
use tracing::debug;

use super::ToolMode;

// System messages
const SYSTEM_MESSAGE_MH: &str = include_str!("../../resources/chat/system_message.md");
const SYSTEM_MESSAGE_TOOLS_MH: &str = include_str!("../../resources/chat/system_message_tools.md");

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
}

impl Agent {
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

    pub async fn get_input_from_ui(&self, prompt: &str) -> Result<String> {
        self.ui.get_input(prompt).await.map_err(|e| e.into())
    }

    async fn run_agent_loop(&mut self) -> Result<()> {
        // Main agent loop
        loop {
            // Get messages from history or create initial ones
            let mut messages = if self.message_history.is_empty() {
                let initial_messages = self.prepare_messages();
                self.message_history = initial_messages.clone();
                initial_messages
            } else {
                self.message_history.clone()
            };

            // Keep trying until all actions succeed
            let mut all_actions_succeeded = false;
            while !all_actions_succeeded {
                let (tool_requests, assistant_msg) = match self
                    .get_next_actions(messages.clone())
                    .await
                {
                    Ok(result) => result,
                    Err(e) => match e {
                        AgentError::LLMError(e) => return Err(e),
                        AgentError::ActionError { error, message } => {
                            messages.push(message.clone());
                            self.message_history.push(message);

                            if let Some(tool_error) = error.downcast_ref::<ToolError>() {
                                match tool_error {
                                    ToolError::UnknownTool(t) => {
                                        let error_msg = Message {
                                            role: MessageRole::User,
                                            content: MessageContent::Text(format!(
                                                "Unknown tool '{}'. Please use only available tools.",
                                                t
                                            )),
                                        };
                                        messages.push(error_msg.clone());
                                        self.message_history.push(error_msg);
                                        continue;
                                    }
                                    ToolError::ParseError(msg) => {
                                        let error_msg = Message {
                                            role: MessageRole::User,
                                            content: MessageContent::Text(format!(
                                                "Tool parameter error: {}. Please try again.",
                                                msg
                                            )),
                                        };
                                        messages.push(error_msg.clone());
                                        self.message_history.push(error_msg);
                                        continue;
                                    }
                                }
                            }
                            return Err(error);
                        }
                    },
                };

                messages.push(assistant_msg.clone());
                self.message_history.push(assistant_msg);

                // Save message history in state
                self.save_state()?;

                // If no tools were requested, get user input
                if tool_requests.is_empty() {
                    // Get input from UI
                    let user_input = self.get_input_from_ui("").await?;

                    // Display the user input as a user message in the UI
                    self.ui
                        .display(UIMessage::UserInput(user_input.clone()))
                        .await?;

                    // Add user input as a new message
                    let user_msg = Message {
                        role: MessageRole::User,
                        content: MessageContent::Text(user_input.clone()),
                    };

                    // No need to add to action history anymore, we use message history

                    // Add the message to history
                    self.message_history.push(user_msg);

                    // Save the state
                    self.save_state()?;

                    // Notify UI of working memory change
                    let _ = self.ui.update_memory(&self.working_memory).await;

                    // Break the inner loop to start a new iteration
                    break;
                }

                all_actions_succeeded = true; // Will be set to false if any action fails

                for tool_request in &tool_requests {
                    let (output, success) = self.execute_tool(tool_request).await?;

                    if !success {
                        all_actions_succeeded = false;
                    }

                    // Add result to messages
                    if self.tool_mode == ToolMode::Native {
                        // Using structured ToolResult for Native mode
                        let message = Message {
                            role: MessageRole::User,
                            content: MessageContent::Structured(vec![ContentBlock::ToolResult {
                                tool_use_id: tool_request.id.clone(),
                                content: output,
                                is_error: if success { None } else { Some(true) },
                            }]),
                        };

                        messages.push(message.clone());
                        self.message_history.push(message);
                    } else {
                        // For XML mode
                        let message = if !success {
                            Message {
                                role: MessageRole::User,
                                content: MessageContent::Text(format!(
                                    "Error executing tool: {}",
                                    output // Use output string directly which will contain error
                                )),
                            }
                        } else {
                            Message {
                                role: MessageRole::User,
                                content: MessageContent::Text(output),
                            }
                        };

                        messages.push(message.clone());
                        self.message_history.push(message);
                    }

                    // Save state
                    self.save_state()?;

                    // Notify UI of working memory change
                    let _ = self.ui.update_memory(&self.working_memory).await;

                    // Check if this was a CompleteTask action
                    if tool_request.name == "complete_task" {
                        // Clean up state file on successful completion
                        self.state_persistence.cleanup()?;
                        debug!("Task completed");
                        return Ok(());
                    }
                }
            }
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
        self.message_history.push(user_msg);

        // Save initial state
        self.save_state()?;

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

        // Note: With the new tool registry approach, we no longer need to manually
        // reconstruct file and web resource state from action history.
        // Instead, the tool outputs themselves (stored in tool_executions)
        // will be used to dynamically generate responses.

        Ok(())
    }

    /// Get the appropriate system prompt based on tool mode
    fn get_system_prompt(&self) -> String {
        let base_prompt = match self.tool_mode {
            ToolMode::Native => SYSTEM_MESSAGE_MH.to_string(),
            ToolMode::Xml => SYSTEM_MESSAGE_TOOLS_MH.to_string(),
        };

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
            return format!("{}\n{}", base_prompt, project_info);
        }

        base_prompt
    }

    /// Get next tool requests from LLM
    async fn get_next_actions(
        &self,
        messages: Vec<Message>,
    ) -> Result<(Vec<ToolRequest>, Message), AgentError> {
        // Inform UI that a new LLM request is starting
        let request_id = self
            .ui
            .begin_llm_request()
            .await
            .map_err(|e| AgentError::LLMError(e.into()))?;
        debug!("Starting LLM request with ID: {}", request_id);

        let request = LLMRequest {
            messages,
            system_prompt: self.get_system_prompt(),
            tools: match self.tool_mode {
                ToolMode::Native => Some(
                    crate::tools::AnnotatedToolDefinition::to_tool_definitions(Tools::all()),
                ),
                ToolMode::Xml => None,
            },
        };

        for (i, message) in request.messages.iter().enumerate() {
            if let MessageContent::Text(text) = &message.content {
                debug!("Message {}: Role={:?}\n---\n{}\n---", i, message.role, text);
            }
        }

        // Create a StreamProcessor and use it to process streaming chunks
        let ui = Arc::clone(&self.ui);
        let processor = Arc::new(Mutex::new(create_stream_processor(self.tool_mode, ui)));

        let streaming_callback: StreamingCallback = Box::new(move |chunk: &StreamingChunk| {
            let mut processor_guard = processor.lock().unwrap();
            processor_guard
                .process(chunk)
                .map_err(|e| anyhow::anyhow!("Failed to process streaming chunk: {}", e))
        });

        let response = self
            .llm_provider
            .send_message(request, Some(&streaming_callback))
            .await?;

        println!("Raw LLM response:");
        for block in &response.content {
            match block {
                ContentBlock::Text { text } => {
                    println!("---\n{}\n---", text);
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    debug!("---\ntool: {}, input: {}\n---", name, input);
                }
                _ => {}
            }
        }
        println!(
            "\n==== Token usage: Input: {}, Output: {}, Cache: Created: {}, Read: {} ====",
            response.usage.input_tokens,
            response.usage.output_tokens,
            response.usage.cache_creation_input_tokens,
            response.usage.cache_read_input_tokens
        );

        let assistant_msg = Message {
            role: MessageRole::Assistant,
            content: MessageContent::Structured(response.content.clone()),
        };

        // Inform UI that the LLM request has completed
        let _ = self.ui.end_llm_request(request_id).await;
        debug!("Completed LLM request with ID: {}", request_id);

        match parse_llm_response(&response, request_id) {
            Ok(tool_requests) => Ok((tool_requests, assistant_msg)),
            Err(e) => Err(AgentError::ActionError {
                error: e,
                message: assistant_msg,
            }),
        }
    }

    /// Prepare initial messages for LLM request
    fn prepare_messages(&self) -> Vec<Message> {
        if self.message_history.is_empty() {
            // Initial message with just the task
            vec![Message {
                role: MessageRole::User,
                content: MessageContent::Text(self.working_memory.current_task.clone()),
            }]
        } else {
            // Return the whole message history
            self.message_history.clone()
        }
    }

    /// Executes a tool and returns the result
    async fn execute_tool(&mut self, tool_request: &ToolRequest) -> Result<(String, bool)> {
        debug!(
            "Executing tool request: {} (id: {})",
            tool_request.name, tool_request.id
        );

        // Update status to Running before execution
        self.ui
            .update_tool_status(&tool_request.id, crate::ui::ToolStatus::Running, None)
            .await?;

        // Get the tool from the registry
        let tool = ToolRegistry::global()
            .get(&tool_request.name)
            .ok_or_else(|| anyhow::anyhow!("Unknown tool: {}", tool_request.name))?;

        // Create a tool context
        let mut context = ToolContext {
            project_manager: self.project_manager.as_ref(),
            command_executor: self.command_executor.as_ref(),
            working_memory: Some(&mut self.working_memory),
        };

        // Execute the tool using the new tool interface
        let result = tool
            .invoke(&mut context, tool_request.input.clone())
            .await?;

        // Determine status based on result
        let status = if result.is_success() {
            crate::ui::ToolStatus::Success
        } else {
            crate::ui::ToolStatus::Error
        };

        // Update tool status with result
        self.ui
            .update_tool_status(&tool_request.id, status, Some(output.clone()))
            .await?;

        // Create and store the ToolExecution record
        let tool_execution = ToolExecution {
            tool_request: tool_request.clone(),
            timestamp: std::time::SystemTime::now(),
            result,
        };

        // Store the execution record
        self.tool_executions.push(tool_execution);

        Ok((output, success))
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

                        // Parse the tool
                        let tool = parse_tool_xml(tool_content)?;

                        // Generate a unique tool ID that matches the one generated by the UI
                        // The UI increments its counter before generating an ID, so we add 1
                        let tool_id = format!("tool-{}-{}", request_id, tool_requests.len() + 1);

                        // Create a ToolRequest
                        let tool_json = serde_json::to_value(&tool).unwrap_or_default();
                        let tool_request = ToolRequest {
                            id: tool_id,
                            name: get_tool_name(&tool),
                            input: tool_json,
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

// Helper function to get a tool name from the Tool enum
fn get_tool_name(tool: &Tool) -> String {
    match tool {
        Tool::ListProjects { .. } => "list_projects",
        Tool::UpdatePlan { .. } => "update_plan",
        Tool::SearchFiles { .. } => "search_files",
        Tool::ExecuteCommand { .. } => "execute_command",
        Tool::ListFiles { .. } => "list_files",
        Tool::ReadFiles { .. } => "read_files",
        Tool::WriteFile { .. } => "write_file",
        Tool::ReplaceInFile { .. } => "replace_in_file",
        Tool::DeleteFiles { .. } => "delete_files",
        Tool::CompleteTask { .. } => "complete_task",
        Tool::UserInput { .. } => "user_input",
        Tool::WebSearch { .. } => "web_search",
        Tool::WebFetch { .. } => "web_fetch",
        Tool::PerplexityAsk { .. } => "perplexity_ask",
    }
    .to_string()
}

fn remove_thinking_tags(input: &str) -> String {
    // First attempt to remove entire <thinking>...</thinking> blocks
    let mut result = String::new();
    let mut current_pos = 0;
    let mut found_any = false;

    while let Some(tag_start) = input[current_pos..].find("<thinking>") {
        let abs_start = current_pos + tag_start;

        // Add text before the <thinking> tag
        result.push_str(&input[current_pos..abs_start]);

        // Find the closing tag
        if let Some(rel_end) = input[abs_start..].find("</thinking>") {
            let abs_end = abs_start + rel_end + "</thinking>".len();
            current_pos = abs_end;
            found_any = true;
        } else {
            // No closing tag found, keep the opening tag and continue
            result.push_str("<thinking>");
            current_pos = abs_start + "<thinking>".len();
        }
    }

    // Add any remaining text
    if current_pos < input.len() {
        result.push_str(&input[current_pos..]);
    }

    let result = result.trim().to_string();

    // If the result is empty or we didn't find any tags, fall back to just removing the tag markers
    if result.is_empty() || !found_any {
        input
            .replace("<thinking>", "")
            .replace("</thinking>", "")
            .trim()
            .to_string()
    } else {
        result
    }
}
