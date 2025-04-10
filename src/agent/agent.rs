use crate::config::ProjectManager;
use crate::llm::{
    ContentBlock, LLMProvider, LLMRequest, Message, MessageContent, MessageRole, StreamingCallback,
    StreamingChunk,
};
use crate::persistence::StatePersistence;
use crate::tools::{
    parse_tool_json, parse_tool_xml, AgentChatToolHandler, AgentToolHandler, ToolExecutor,
    TOOL_TAG_PREFIX,
};
use crate::types::*;
use crate::ui::{streaming::create_stream_processor, UIMessage, UserInterface};
use crate::utils::CommandExecutor;
use anyhow::Result;
use percent_encoding;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::debug;

use super::{AgentMode, ToolMode};

// System messages for WorkingMemory mode
const SYSTEM_MESSAGE_WM: &str = include_str!("../../resources/working_memory/system_message.md");
const SYSTEM_MESSAGE_TOOLS_WM: &str =
    include_str!("../../resources/working_memory/system_message_tools.md");

// System messages for MessageHistory mode
const SYSTEM_MESSAGE_MH: &str = include_str!("../../resources/chat/system_message.md");
const SYSTEM_MESSAGE_TOOLS_MH: &str = include_str!("../../resources/chat/system_message_tools.md");

pub struct Agent {
    working_memory: WorkingMemory,
    llm_provider: Box<dyn LLMProvider>,
    tool_mode: ToolMode,
    agent_mode: AgentMode,
    project_manager: Box<dyn ProjectManager>,
    command_executor: Box<dyn CommandExecutor>,
    ui: Arc<Box<dyn UserInterface>>,
    state_persistence: Box<dyn StatePersistence>,
    // For MessageHistory mode: store all messages exchanged
    message_history: Vec<Message>,
    // Path provided during agent initialization
    init_path: Option<PathBuf>,
    // Name of the initial project (if any)
    initial_project: Option<String>,
}

impl Agent {
    pub fn new(
        llm_provider: Box<dyn LLMProvider>,
        tool_mode: ToolMode,
        agent_mode: AgentMode,
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
            agent_mode,
            project_manager,
            ui: Arc::new(ui),
            command_executor,
            state_persistence,
            message_history: Vec::new(),
            init_path,
            initial_project: None,
        }
    }

    /// Helper method to save the state based on the current agent mode
    fn save_state_based_on_mode(&mut self) -> Result<()> {
        match self.agent_mode {
            AgentMode::WorkingMemory => {
                self.state_persistence.save_state(
                    self.working_memory.current_task.clone(),
                    self.working_memory.action_history.clone(),
                )?;
            }
            AgentMode::MessageHistory => {
                self.state_persistence.save_state_with_messages(
                    self.working_memory.current_task.clone(),
                    self.working_memory.action_history.clone(),
                    self.message_history.clone(),
                )?;
            }
        }
        Ok(())
    }

    pub async fn get_input_from_ui(&self, prompt: &str) -> Result<String> {
        self.ui.get_input(prompt).await.map_err(|e| e.into())
    }

    async fn run_agent_loop(&mut self) -> Result<()> {
        // Main agent loop
        loop {
            // Get messages based on the agent mode
            let mut messages = match self.agent_mode {
                AgentMode::WorkingMemory => self.prepare_messages(),
                AgentMode::MessageHistory => {
                    if self.message_history.is_empty() {
                        let initial_messages = self.prepare_messages();
                        self.message_history = initial_messages.clone();
                        initial_messages
                    } else {
                        self.message_history.clone()
                    }
                }
            };

            // Keep trying until all actions succeed
            let mut all_actions_succeeded = false;
            while !all_actions_succeeded {
                let (actions, assistant_msg) = match self.get_next_actions(messages.clone()).await {
                    Ok(result) => result,
                    Err(e) => match e {
                        AgentError::LLMError(e) => return Err(e),
                        AgentError::ActionError { error, message } => {
                            messages.push(message.clone());
                            if self.agent_mode == AgentMode::MessageHistory {
                                self.message_history.push(message);
                            }

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
                                        if self.agent_mode == AgentMode::MessageHistory {
                                            self.message_history.push(error_msg);
                                        }
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
                                        if self.agent_mode == AgentMode::MessageHistory {
                                            self.message_history.push(error_msg);
                                        }
                                        continue;
                                    }
                                }
                            }
                            return Err(error);
                        }
                    },
                };

                messages.push(assistant_msg.clone());
                if self.agent_mode == AgentMode::MessageHistory {
                    self.message_history.push(assistant_msg);

                    // Save message history in state
                    self.state_persistence.save_state_with_messages(
                        self.working_memory.current_task.clone(),
                        self.working_memory.action_history.clone(),
                        self.message_history.clone(),
                    )?;
                }

                // If no actions were returned, get user input
                if actions.is_empty() {
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

                    // Add user input as an action result to working memory
                    let action_result = ActionResult {
                        tool: Tool::UserInput {},
                        result: ToolResult::UserInput {
                            message: user_input,
                        },
                        reasoning: "User provided input".to_string(),
                    };

                    self.working_memory.action_history.push(action_result);

                    // For MessageHistory mode, add the message to history
                    if self.agent_mode == AgentMode::MessageHistory {
                        self.message_history.push(user_msg);
                    }

                    // Save the state based on current mode
                    self.save_state_based_on_mode()?;

                    // Notify UI of working memory change
                    let _ = self.ui.update_memory(&self.working_memory).await;

                    // Break the inner loop to start a new iteration
                    break;
                }

                all_actions_succeeded = true; // Will be set to false if any action fails

                for action in actions {
                    let (output, result) = self.execute_action(&action).await?;
                    let success = result.result.is_success();

                    if !success {
                        all_actions_succeeded = false;
                    }

                    // Add result to working memory
                    self.working_memory.action_history.push(result.clone());

                    // Add result to messages for both modes
                    if self.tool_mode == ToolMode::Native {
                        // Using structured ToolResult for Native mode
                        let message = Message {
                            role: MessageRole::User,
                            content: MessageContent::Structured(vec![ContentBlock::ToolResult {
                                tool_use_id: action.tool_id.clone(),
                                content: output,
                                is_error: if success { None } else { Some(true) },
                            }]),
                        };

                        messages.push(message.clone());
                        if self.agent_mode == AgentMode::MessageHistory {
                            self.message_history.push(message);
                        }
                    } else {
                        // For XML mode
                        let message = if !success {
                            Message {
                                role: MessageRole::User,
                                content: MessageContent::Text(format!(
                                    "Error executing tool: {}",
                                    result.result.format_message()
                                )),
                            }
                        } else {
                            Message {
                                role: MessageRole::User,
                                content: MessageContent::Text(output),
                            }
                        };

                        messages.push(message.clone());
                        if self.agent_mode == AgentMode::MessageHistory {
                            self.message_history.push(message);
                        }
                    }

                    // In WorkingMemory mode, stop processing remaining actions on failure
                    // But in MessageHistory mode, continue processing all actions
                    if !success && self.agent_mode == AgentMode::WorkingMemory {
                        break;
                    }

                    // Save state based on mode
                    // Save the state based on current mode
                    self.save_state_based_on_mode()?;

                    // Notify UI of working memory change
                    let _ = self.ui.update_memory(&self.working_memory).await;

                    // Check if this was a CompleteTask action
                    if let Tool::CompleteTask { .. } = action.tool {
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

        // For message history mode, create the initial user message
        if self.agent_mode == AgentMode::MessageHistory {
            let user_msg = Message {
                role: MessageRole::User,
                content: MessageContent::Text(task.clone()),
            };
            self.message_history.push(user_msg);
        }

        // Save initial state based on agent mode
        self.save_state_based_on_mode()?;

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

            // Restore action history from saved state
            self.working_memory.action_history = state.actions.clone();

            // For MessageHistory mode, restore messages if available
            if let Some(messages) = state.messages {
                self.message_history = messages;
                debug!("Restored {} previous messages", self.message_history.len());
            } else if self.agent_mode == AgentMode::MessageHistory {
                // If no messages were saved but we're in MessageHistory mode,
                // create an initial message with the task
                self.message_history = vec![Message {
                    role: MessageRole::User,
                    content: MessageContent::Text(state.task.clone()),
                }];
            }

            // Load current state of files into memory - will create file trees as needed
            self.load_current_files_to_memory().await?;

            self.ui
                .display(UIMessage::Action(format!(
                    "Continuing task: {}, loaded {} previous actions",
                    state.task,
                    state.actions.len()
                )))
                .await?;

            // Notify UI of loaded working memory
            let _ = self.ui.update_memory(&self.working_memory).await;

            self.run_agent_loop().await
        } else {
            anyhow::bail!("No saved state found")
        }
    }

    /// Load all currently existing files and web resources into working memory based on action history
    async fn load_current_files_to_memory(&mut self) -> Result<()> {
        // Group files by project and organize paths that should exist
        let mut project_files: HashMap<String, (HashSet<PathBuf>, Vec<String>)> = HashMap::new();

        // First pass: Handle files
        for action in &self.working_memory.action_history {
            let project = match &action.tool {
                Tool::WriteFile { project, path, .. } => {
                    let project_entry = project_files
                        .entry(project.clone())
                        .or_insert_with(|| (HashSet::new(), Vec::new()));
                    project_entry.0.insert(path.clone());
                    Some(project)
                }
                Tool::ReplaceInFile { project, path, .. } => {
                    let project_entry = project_files
                        .entry(project.clone())
                        .or_insert_with(|| (HashSet::new(), Vec::new()));
                    project_entry.0.insert(path.clone());
                    Some(project)
                }
                Tool::ReadFiles { project, paths, .. } => {
                    let project_entry = project_files
                        .entry(project.clone())
                        .or_insert_with(|| (HashSet::new(), Vec::new()));
                    for path in paths {
                        project_entry.0.insert(path.clone());
                    }
                    Some(project)
                }
                Tool::DeleteFiles { project, paths, .. } => {
                    let project_entry = project_files
                        .entry(project.clone())
                        .or_insert_with(|| (HashSet::new(), Vec::new()));
                    for path in paths {
                        project_entry.0.remove(path);
                    }
                    Some(project)
                }
                _ => None,
            };

            // If this action might have errors for a specific project, record it
            if let Some(proj) = project {
                if let ToolResult::ReadFiles { failed_files, .. } = &action.result {
                    if !failed_files.is_empty() {
                        project_files.get_mut(proj).unwrap().1.push(format!(
                            "Skipping failed files: {}",
                            failed_files
                                .iter()
                                .map(|(path, _)| path.display().to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                }
            }
        }

        // Load files for each project
        for (project_name, (files, errors)) in project_files {
            // Get explorer for this project
            match self.project_manager.get_explorer_for_project(&project_name) {
                Ok(explorer) => {
                    let root_dir = explorer.root_dir();

                    // Create file tree for this project if it doesn't exist yet
                    if !self.working_memory.file_trees.contains_key(&project_name) {
                        let mut explorer_for_tree = self
                            .project_manager
                            .get_explorer_for_project(&project_name)?;
                        match explorer_for_tree.create_initial_tree(2) {
                            Ok(tree) => {
                                self.working_memory
                                    .file_trees
                                    .insert(project_name.clone(), tree);
                                // Add to available projects if not already there
                                if !self
                                    .working_memory
                                    .available_projects
                                    .contains(&project_name)
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

                    // Load files into memory
                    for path in files {
                        let full_path = if path.is_absolute() {
                            path.clone()
                        } else {
                            root_dir.join(&path)
                        };

                        match explorer.read_file(&full_path) {
                            Ok(content) => {
                                debug!(
                                    "Loading existing file from project {}: {}",
                                    project_name,
                                    full_path.display()
                                );
                                self.working_memory.add_resource(
                                    project_name.clone(),
                                    path,
                                    LoadedResource::File(content),
                                );
                            }
                            Err(e) => {
                                debug!("Error loading file {}: {}", full_path.display(), e);
                            }
                        }
                    }
                }
                Err(e) => {
                    debug!("Error getting explorer for project {}: {}", project_name, e);
                }
            }

            // Add any errors to working memory
            for error in errors {
                debug!("Errors for project {}: {}", project_name, error);
            }
        }

        // Second pass: Handle web resources from action results
        // This must be done after the file processing to avoid losing web resources
        // that were added in the same continuation session
        for action in &self.working_memory.action_history {
            match &action.result {
                ToolResult::WebSearch {
                    query,
                    results,
                    error: None,
                } => {
                    // Use a synthetic path that includes the query (same as in AgentToolHandler)
                    let path = PathBuf::from(format!(
                        "web-search-{}",
                        percent_encoding::utf8_percent_encode(
                            &query,
                            percent_encoding::NON_ALPHANUMERIC
                        )
                    ));
                    debug!("Loading web search results for: {}", query);
                    // Use "web" as the project name for web resources
                    let project = "web".to_string();
                    self.working_memory.loaded_resources.insert(
                        (project, path),
                        LoadedResource::WebSearch {
                            query: query.clone(),
                            results: results.clone(),
                        },
                    );
                }
                ToolResult::WebFetch { page, error: None } => {
                    // Use the URL as path (normalized, same as in AgentToolHandler)
                    let path = PathBuf::from(page.url.replace([':', '/', '?', '#'], "_"));
                    debug!("Loading web page content: {}", page.url);
                    // Use "web" as the project name for web resources
                    let project = "web".to_string();
                    self.working_memory
                        .loaded_resources
                        .insert((project, path), LoadedResource::WebPage(page.clone()));
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// Get the appropriate system prompt based on agent mode and tool mode
    fn get_system_prompt(&self) -> String {
        let base_prompt = match self.agent_mode {
            AgentMode::WorkingMemory => match self.tool_mode {
                ToolMode::Native => SYSTEM_MESSAGE_WM.to_string(),
                ToolMode::Xml => SYSTEM_MESSAGE_TOOLS_WM.to_string(),
            },
            AgentMode::MessageHistory => match self.tool_mode {
                ToolMode::Native => SYSTEM_MESSAGE_MH.to_string(),
                ToolMode::Xml => SYSTEM_MESSAGE_TOOLS_MH.to_string(),
            },
        };

        // In MessageHistory mode, append project information to the system prompt
        if self.agent_mode == AgentMode::MessageHistory {
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
        }

        base_prompt
    }

    /// Get next actions from LLM
    async fn get_next_actions(
        &self,
        messages: Vec<Message>,
    ) -> Result<(Vec<AgentAction>, Message), AgentError> {
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
                ToolMode::Native => Some(Tools::all()),
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
            Ok(actions) => Ok((actions, assistant_msg)),
            Err(e) => Err(AgentError::ActionError {
                error: e,
                message: assistant_msg,
            }),
        }
    }

    /// Prepare messages for LLM request based on the agent mode
    fn prepare_messages(&self) -> Vec<Message> {
        match self.agent_mode {
            AgentMode::WorkingMemory => {
                // Single message with working memory
                vec![Message {
                    role: MessageRole::User,
                    content: MessageContent::Text(self.working_memory.to_markdown()),
                }]
            }
            AgentMode::MessageHistory => {
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
        }
    }

    /// Executes an action and returns the result
    async fn execute_action(&mut self, action: &AgentAction) -> Result<(String, ActionResult)> {
        debug!("Executing action: {:?}", action.tool);

        // Update status to Running before execution
        self.ui
            .update_tool_status(&action.tool_id, crate::ui::ToolStatus::Running, None)
            .await?;

        // Execute the tool and get both the output and result based on agent mode
        let (output, tool_result) = match self.agent_mode {
            AgentMode::WorkingMemory => {
                let mut handler = AgentToolHandler::new(&mut self.working_memory);
                ToolExecutor::execute(
                    &mut handler,
                    &self.project_manager,
                    &self.command_executor,
                    Some(&self.ui),
                    &action.tool,
                )
                .await?
            }
            AgentMode::MessageHistory => {
                let mut handler = AgentChatToolHandler::new(&mut self.working_memory);
                ToolExecutor::execute(
                    &mut handler,
                    &self.project_manager,
                    &self.command_executor,
                    Some(&self.ui),
                    &action.tool,
                )
                .await?
            }
        };

        // Determine status based on result
        let status = if tool_result.is_success() {
            crate::ui::ToolStatus::Success
        } else {
            crate::ui::ToolStatus::Error
        };

        // Update tool status with result
        self.ui
            .update_tool_status(&action.tool_id, status, Some(output.clone()))
            .await?;

        let action_result = ActionResult {
            tool: action.tool.clone(),
            result: tool_result,
            reasoning: action.reasoning.clone(),
        };

        Ok((output, action_result))
    }
}

pub(crate) fn parse_llm_response(
    response: &crate::llm::LLMResponse,
    request_id: u64,
) -> Result<Vec<AgentAction>> {
    let mut actions = Vec::new();

    let mut reasoning = String::new();

    for block in &response.content {
        if let ContentBlock::Text { text } = block {
            let mut current_pos = 0;

            while let Some(tool_start) = text[current_pos..].find(&format!("<{}", TOOL_TAG_PREFIX))
            {
                let abs_start = current_pos + tool_start;

                // Add text before tool to reasoning
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

                        // Parse and add the tool action
                        let tool = parse_tool_xml(tool_content)?;
                        // Generate a unique tool ID that matches the one generated by the UI
                        // The UI increments its counter before generating an ID, so we add 1
                        let tool_id = format!("tool-{}-{}", request_id, actions.len() + 1);
                        actions.push(AgentAction {
                            tool,
                            reasoning: remove_thinking_tags(&reasoning).to_owned(),
                            tool_id,
                        });

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

        if let ContentBlock::ToolUse {
            name, input, id, ..
        } = block
        {
            let tool = parse_tool_json(name, input)?;
            // Generate a tool ID - either use the provided one or create a new one
            // Note: for API-native tools, this ID is important as it will be used to match with tool_result
            let tool_id = if id.is_empty() {
                format!("tool-json-{}", actions.len())
            } else {
                id.clone()
            };
            actions.push(AgentAction {
                tool,
                reasoning: remove_thinking_tags(&reasoning).to_owned(),
                tool_id,
            });
            reasoning = String::new();
        }
    }

    Ok(actions)
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
