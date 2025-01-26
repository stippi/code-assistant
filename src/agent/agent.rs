use crate::llm::{ContentBlock, LLMProvider, LLMRequest, Message, MessageContent, MessageRole};
use crate::persistence::StatePersistence;
use crate::tool_definitions::Tools;
use crate::types::*;
use crate::ui::{UIMessage, UserInterface};
use crate::utils::{format_with_line_numbers, CommandExecutor};
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, warn};

const SYSTEM_MESSAGE: &str = include_str!("../../resources/system_message.md");

fn get_system_message(replacements: &HashMap<&str, String>) -> String {
    let mut message = String::from(SYSTEM_MESSAGE);
    for (key, value) in replacements {
        message = message.replace(key, value);
    }
    message
}

pub enum ToolMode {
    Native,
    Xml,
}

pub struct Agent {
    working_memory: WorkingMemory,
    llm_provider: Box<dyn LLMProvider>,
    tool_mode: ToolMode,
    explorer: Box<dyn CodeExplorer>,
    command_executor: Box<dyn CommandExecutor>,
    ui: Box<dyn UserInterface>,
    state_persistence: Box<dyn StatePersistence>,
}

impl Agent {
    pub fn new(
        llm_provider: Box<dyn LLMProvider>,
        tool_mode: ToolMode,
        explorer: Box<dyn CodeExplorer>,
        command_executor: Box<dyn CommandExecutor>,
        ui: Box<dyn UserInterface>,
        state_persistence: Box<dyn StatePersistence>,
    ) -> Self {
        Self {
            working_memory: WorkingMemory::default(),
            llm_provider,
            tool_mode,
            explorer,
            ui,
            command_executor,
            state_persistence,
        }
    }

    async fn run_agent_loop(&mut self) -> Result<()> {
        // Main agent loop
        loop {
            let actions = self.get_next_actions().await?;

            for action in actions {
                let result = self.execute_action(&action).await?;
                self.working_memory.action_history.push(result);

                // Save state after each action
                self.state_persistence.save_state(
                    self.working_memory.current_task.clone(),
                    self.working_memory.action_history.clone(),
                )?;

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

    /// Start a new agent task
    pub async fn start_with_task(&mut self, task: String) -> Result<()> {
        debug!("Starting agent with task: {}", task);
        self.working_memory.current_task = task.clone();

        self.ui
            .display(UIMessage::Action(
                "Creating initial repository structure...".to_string(),
            ))
            .await?;

        self.working_memory.file_tree = Some(self.explorer.create_initial_tree(2)?);

        // Save initial state
        self.state_persistence
            .save_state(task, self.working_memory.action_history.clone())?;

        self.run_agent_loop().await
    }

    /// Continue from a saved state
    pub async fn start_from_state(&mut self) -> Result<()> {
        if let Some(state) = self.state_persistence.load_state()? {
            debug!("Continuing task: {}", state.task);
            self.working_memory.current_task = state.task;

            // Create fresh working memory
            self.working_memory.file_tree = Some(self.explorer.create_initial_tree(2)?);

            self.ui
                .display(UIMessage::Action(format!(
                    "Continuing task: {}, replaying {} actions",
                    self.working_memory.current_task,
                    state.actions.len()
                )))
                .await?;

            // Replay each action
            for original_action in state.actions {
                debug!("Replaying action: {:?}", original_action.tool);
                let action = AgentAction {
                    tool: original_action.tool.clone(),
                    reasoning: original_action.reasoning.clone(),
                };

                match self.execute_action(&action).await {
                    Ok(result) => {
                        if !result.success {
                            warn!(
                                "Action replay failed: {:?}. Using original result instead.",
                                result.error
                            );
                            // Use the original successful result instead
                            self.working_memory.action_history.push(original_action);
                        } else {
                            self.working_memory.action_history.push(result);
                        }
                    }
                    Err(e) => {
                        warn!(
                            "Failed to replay action: {}. Using original result instead.",
                            e
                        );
                        // Use the original result in case of error
                        self.working_memory.action_history.push(original_action);
                    }
                }
            }

            self.run_agent_loop().await
        } else {
            anyhow::bail!("No saved state found")
        }
    }

    /// Get next actions from LLM
    async fn get_next_actions(&self) -> Result<Vec<AgentAction>> {
        let messages = self.prepare_messages();

        let replacements = HashMap::new();
        // replacements.insert("${TOOL_TAG_PREFIX}", TOOL_TAG_PREFIX.to_string());
        let system_message = get_system_message(&replacements);

        let request = LLMRequest {
            messages,
            system_prompt: system_message,
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

        let response = self.llm_provider.send_message(request).await?;

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

        parse_llm_response(&response)
    }

    pub fn render_working_memory(&self) -> String {
        let mut memory = format!("Task: {}\n\n", self.working_memory.current_task);

        // Add repository structure with proper indentation
        memory.push_str("Repository structure:\n");
        if let Some(tree) = &self.working_memory.file_tree {
            memory.push_str(&tree.to_string());
        } else {
            memory.push_str("No file tree available");
        }
        memory.push_str("\n\n");

        // Add loaded files with their contents
        memory.push_str("Current Working Memory:\n");
        memory.push_str("- Loaded files and their contents (with line numbers prepended):\n");
        for (path, content) in &self.working_memory.loaded_files {
            memory.push_str(&format!(
                "\n-----{}:\n{}\n",
                path.display(),
                format_with_line_numbers(content)
            ));
        }

        // Add file summaries
        memory.push_str("\n- File summaries:\n");
        for (path, summary) in &self.working_memory.file_summaries {
            memory.push_str(&format!("  {}: {}\n", path.display(), summary));
        }

        // Add action history
        memory.push_str("\nPrevious actions:\n");
        for (i, action) in self.working_memory.action_history.iter().enumerate() {
            memory.push_str(&format!("\n{}. Tool: {:?}\n", i + 1, action.tool));
            memory.push_str(&format!("   Reasoning: {}\n", action.reasoning));
            memory.push_str(&format!("   Result: {}\n", action.result));
            if let Some(error) = &action.error {
                memory.push_str(&format!("   Error: {}\n", error));
            }
        }

        memory
    }

    /// Prepare messages for LLM request - currently returns a single user message
    /// but kept as Vec<Message> for flexibility to change the format later
    fn prepare_messages(&self) -> Vec<Message> {
        vec![Message {
            role: MessageRole::User,
            content: MessageContent::Text(self.render_working_memory()),
        }]
    }

    /// Executes an action and returns the result
    async fn execute_action(&mut self, action: &AgentAction) -> Result<ActionResult> {
        debug!("Executing action: {:?}", action.tool);

        // Display the agent's reasoning
        self.ui
            .display(UIMessage::Reasoning(action.reasoning.clone()))
            .await?;

        let result = match &action.tool {
            Tool::ListFiles { paths, max_depth } => {
                let mut expanded_paths = Vec::new();
                let mut failed_paths = Vec::new();

                for path in paths {
                    self.ui
                        .display(UIMessage::Action(format!(
                            "Listing contents of `{}`",
                            path.display()
                        )))
                        .await?;

                    let full_path = if path.is_absolute() {
                        path.clone()
                    } else {
                        self.explorer.root_dir().join(path)
                    };

                    match self.explorer.list_files(&full_path, *max_depth) {
                        Ok(tree_entry) => {
                            // Update the file tree with the new expanded entry
                            if let Some(ref mut file_tree) = self.working_memory.file_tree {
                                update_tree_entry(file_tree, path, tree_entry)?;
                            }
                            expanded_paths.push(path.display().to_string());
                        }
                        Err(e) => {
                            failed_paths.push((path.display().to_string(), e.to_string()));
                        }
                    }
                }

                let result_message = if !expanded_paths.is_empty() {
                    format!(
                        "Successfully listed contents of: {}",
                        expanded_paths.join(", ")
                    )
                } else {
                    String::new()
                };

                let error_message = if !failed_paths.is_empty() {
                    Some(
                        failed_paths
                            .iter()
                            .map(|(path, err)| format!("{}: {}", path, err))
                            .collect::<Vec<_>>()
                            .join("; "),
                    )
                } else {
                    None
                };

                ActionResult {
                    tool: action.tool.clone(),
                    success: !expanded_paths.is_empty(),
                    result: result_message,
                    error: error_message,
                    reasoning: action.reasoning.clone(),
                }
            }

            Tool::ReadFiles { paths } => {
                let mut loaded_files = Vec::new();
                let mut failed_files = Vec::new();

                for path in paths {
                    self.ui
                        .display(UIMessage::Action(format!(
                            "Reading file `{}`",
                            path.display()
                        )))
                        .await?;

                    let full_path = if path.is_absolute() {
                        path.clone()
                    } else {
                        self.explorer.root_dir().join(path)
                    };

                    match self.explorer.read_file(&full_path) {
                        Ok(content) => {
                            self.working_memory
                                .loaded_files
                                .insert(path.clone(), content);
                            loaded_files.push(path.display().to_string());
                        }
                        Err(e) => {
                            failed_files.push((path.display().to_string(), e.to_string()));
                        }
                    }
                }

                let result_message = if !loaded_files.is_empty() {
                    format!("Successfully loaded files: {}", loaded_files.join(", "))
                } else {
                    String::from("No files loaded")
                };

                let error_message = if !failed_files.is_empty() {
                    Some(
                        failed_files
                            .iter()
                            .map(|(path, err)| format!("{}: {}", path, err))
                            .collect::<Vec<_>>()
                            .join("; "),
                    )
                } else {
                    None
                };

                ActionResult {
                    tool: action.tool.clone(),
                    success: !loaded_files.is_empty(),
                    result: result_message,
                    error: error_message,
                    reasoning: action.reasoning.clone(),
                }
            }

            Tool::WriteFile { path, content } => {
                self.ui
                    .display(UIMessage::Action(format!(
                        "Writing file `{}`",
                        path.display()
                    )))
                    .await?;

                let full_path = if path.is_absolute() {
                    path.clone()
                } else {
                    self.explorer.root_dir().join(path)
                };

                // Ensure the parent directory exists
                if let Some(parent) = full_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }

                match std::fs::write(&full_path, content) {
                    Ok(_) => ActionResult {
                        tool: action.tool.clone(),
                        success: true,
                        result: format!("Successfully wrote to {}", full_path.display()),
                        error: None,
                        reasoning: action.reasoning.clone(),
                    },
                    Err(e) => ActionResult {
                        tool: action.tool.clone(),
                        success: false,
                        result: String::new(),
                        error: Some(e.to_string()),
                        reasoning: action.reasoning.clone(),
                    },
                }
            }

            Tool::UpdateFile { path, updates } => {
                self.ui
                    .display(UIMessage::Action(format!(
                        "Updating {} sections in `{}`",
                        updates.len(),
                        path.display()
                    )))
                    .await?;

                let full_path = if path.is_absolute() {
                    path.clone()
                } else {
                    self.explorer.root_dir().join(path)
                };

                match self.explorer.apply_updates(&full_path, updates) {
                    Ok(new_content) => {
                        // Write the updated file
                        std::fs::write(&full_path, new_content.clone())?;

                        // Also update the working memory in case it is currently loaded there
                        if let Some(old_content) = self.working_memory.loaded_files.get_mut(path) {
                            *old_content = new_content;
                        }

                        ActionResult {
                            tool: action.tool.clone(),
                            success: true,
                            result: format!(
                                "Successfully applied {} updates to {}",
                                updates.len(),
                                path.display()
                            ),
                            error: None,
                            reasoning: action.reasoning.clone(),
                        }
                    }
                    Err(e) => ActionResult {
                        tool: action.tool.clone(),
                        success: false,
                        result: String::new(),
                        error: Some(e.to_string()),
                        reasoning: action.reasoning.clone(),
                    },
                }
            }

            Tool::Summarize { files } => {
                self.ui
                    .display(UIMessage::Action(format!(
                        "Summarizing {} files",
                        files.len()
                    )))
                    .await?;

                for (path, summary) in files {
                    self.working_memory.loaded_files.remove(path);
                    self.working_memory
                        .file_summaries
                        .insert(path.clone(), summary.clone());
                }

                ActionResult {
                    tool: action.tool.clone(),
                    success: true,
                    result: format!(
                        "Summarized {} files and updated working memory",
                        files.len()
                    ),
                    error: None,
                    reasoning: action.reasoning.clone(),
                }
            }

            Tool::AskUser { question } => {
                // Display the question
                self.ui
                    .display(UIMessage::Question(question.clone()))
                    .await?;

                // Get the response
                match self.ui.get_input("> ").await {
                    Ok(response) => ActionResult {
                        tool: action.tool.clone(),
                        success: true,
                        result: response,
                        error: None,
                        reasoning: action.reasoning.clone(),
                    },
                    Err(e) => ActionResult {
                        tool: action.tool.clone(),
                        success: false,
                        result: String::new(),
                        error: Some(e.to_string()),
                        reasoning: action.reasoning.clone(),
                    },
                }
            }

            Tool::MessageUser { message } => {
                self.ui
                    .display(UIMessage::Action(format!("Message: {}", message)))
                    .await?;

                ActionResult {
                    tool: action.tool.clone(),
                    success: true,
                    result: format!("Message delivered"),
                    error: None,
                    reasoning: action.reasoning.clone(),
                }
            }

            Tool::ExecuteCommand {
                command_line,
                working_dir,
            } => {
                self.ui
                    .display(UIMessage::Action(format!(
                        "Executing command: {}",
                        command_line
                    )))
                    .await?;

                match self
                    .command_executor
                    .execute(&command_line, working_dir.as_ref())
                    .await
                {
                    Ok(output) => {
                        let mut result = String::new();
                        if !output.stdout.is_empty() {
                            result.push_str("Output:\n");
                            result.push_str(&output.stdout);
                        }
                        if !output.stderr.is_empty() {
                            if !result.is_empty() {
                                result.push_str("\n");
                            }
                            result.push_str("Errors:\n");
                            result.push_str(&output.stderr);
                        }

                        ActionResult {
                            tool: action.tool.clone(),
                            success: output.success,
                            result,
                            error: if output.success {
                                None
                            } else {
                                Some("Command failed".to_string())
                            },
                            reasoning: action.reasoning.clone(),
                        }
                    }
                    Err(e) => ActionResult {
                        tool: action.tool.clone(),
                        success: false,
                        result: String::new(),
                        error: Some(format!("Failed to execute command: {}", e)),
                        reasoning: action.reasoning.clone(),
                    },
                }
            }

            Tool::DeleteFiles { paths } => {
                let mut deleted_files = Vec::new();
                let mut failed_files = Vec::new();
                for path in paths {
                    self.ui
                        .display(UIMessage::Action(format!(
                            "Deleting file `{}`",
                            path.display()
                        )))
                        .await?;
                    let full_path = if path.is_absolute() {
                        path.clone()
                    } else {
                        self.explorer.root_dir().join(path)
                    };
                    match std::fs::remove_file(&full_path) {
                        Ok(_) => {
                            deleted_files.push(path.display().to_string());
                            // Remove from working memory if it was loaded
                            self.working_memory.loaded_files.remove(path);
                            self.working_memory.file_summaries.remove(path);
                        }
                        Err(e) => {
                            failed_files.push((path.display().to_string(), e.to_string()));
                        }
                    }
                }
                let result_message = if !deleted_files.is_empty() {
                    format!("Successfully deleted files: {}", deleted_files.join(", "))
                } else {
                    String::from("No files were deleted")
                };
                let error_message = if !failed_files.is_empty() {
                    Some(
                        failed_files
                            .iter()
                            .map(|(path, err)| format!("{}: {}", path, err))
                            .collect::<Vec<_>>()
                            .join("; "),
                    )
                } else {
                    None
                };
                ActionResult {
                    tool: action.tool.clone(),
                    success: !deleted_files.is_empty(),
                    result: result_message,
                    error: error_message,
                    reasoning: action.reasoning.clone(),
                }
            }

            Tool::SearchFiles {
                query,
                path,
                case_sensitive,
                whole_words,
                regex_mode,
                max_results,
            } => {
                let search_path = if let Some(p) = path {
                    if p.is_absolute() {
                        p.clone()
                    } else {
                        self.explorer.root_dir().join(p)
                    }
                } else {
                    self.explorer.root_dir()
                };

                self.ui
                    .display(UIMessage::Action(format!(
                        "Searching for '{}' in {}",
                        query,
                        search_path.display()
                    )))
                    .await?;

                let options = SearchOptions {
                    query: query.clone(),
                    case_sensitive: *case_sensitive,
                    whole_words: *whole_words,
                    mode: if *regex_mode {
                        SearchMode::Regex
                    } else {
                        SearchMode::Exact
                    },
                    max_results: *max_results,
                };

                match self.explorer.search(&search_path, options) {
                    Ok(results) => {
                        let mut output = String::new();
                        for result in &results {
                            output.push_str(&format!(
                                "{}:{}:{}\n",
                                result.file.display(),
                                result.line_number,
                                result.line_content
                            ));
                        }

                        ActionResult {
                            tool: action.tool.clone(),
                            success: true,
                            result: if results.is_empty() {
                                "No matches found".to_string()
                            } else {
                                format!("Found {} matches:\n{}", results.len(), output)
                            },
                            error: None,
                            reasoning: action.reasoning.clone(),
                        }
                    }
                    Err(e) => ActionResult {
                        tool: action.tool.clone(),
                        success: false,
                        result: String::new(),
                        error: Some(format!("Search failed: {}", e)),
                        reasoning: action.reasoning.clone(),
                    },
                }
            }

            Tool::CompleteTask { message } => {
                self.ui
                    .display(UIMessage::Action(format!("Task completed: {}", message)))
                    .await?;

                ActionResult {
                    tool: action.tool.clone(),
                    success: true,
                    result: "Task completed".to_string(),
                    error: None,
                    reasoning: action.reasoning.clone(),
                }
            }
        };

        // Log the result
        if result.success {
            debug!("Action execution successful: {:?}", result.tool);
        } else {
            warn!(
                "Action execution failed: {:?}, error: {:?}",
                result.tool, result.error
            );
        }

        Ok(result)
    }
}

const TOOL_TAG_PREFIX: &str = "tool:";
const PARAM_TAG_PREFIX: &str = "param:";

pub(crate) fn parse_llm_response(response: &crate::llm::LLMResponse) -> Result<Vec<AgentAction>> {
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
                        actions.push(AgentAction {
                            tool,
                            reasoning: reasoning.trim().to_string(),
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

        if let ContentBlock::ToolUse { name, input, .. } = block {
            let tool = parse_tool_json(name, input)?;
            actions.push(AgentAction {
                tool,
                reasoning: reasoning.trim().to_string(),
            });
            reasoning = String::new();
        }
    }

    Ok(actions)
}

fn parse_tool_xml(xml: &str) -> Result<Tool> {
    debug!("Parsing XML:\n{}", xml);

    let tool_name = xml
        .trim()
        .strip_prefix(&format!("<{}", TOOL_TAG_PREFIX))
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.strip_suffix('>'))
        .ok_or_else(|| anyhow::anyhow!("Missing tool name"))?
        .to_string();

    debug!("Found tool name: {}", tool_name);

    let mut params = HashMap::new();
    let mut current_param = String::new();
    let mut current_value = String::new();

    for line in xml.lines() {
        let line = line.trim();
        debug!("Processing line: '{}'", line);

        if line.is_empty()
            || line == format!("<{}{}>", TOOL_TAG_PREFIX, tool_name)
            || line == format!("</{}{}>", TOOL_TAG_PREFIX, tool_name)
        {
            debug!("Skipping tool tag line");
            continue;
        }

        // Check for parameter start with prefix
        if let Some(param_start) = line.strip_prefix(&format!("<{}", PARAM_TAG_PREFIX)) {
            if !param_start.starts_with('/') {
                // Ignore closing tags
                if let Some(param_name) = param_start.split('>').next() {
                    current_param = param_name.to_string();
                    debug!("Found parameter start: {}", current_param);

                    // Check if it's a single-line parameter
                    if line.contains(&format!("</{}{}>", PARAM_TAG_PREFIX, current_param)) {
                        // Find positions for start/end tags
                        let content_start = line
                            .find('>')
                            .map(|pos| pos + 1)
                            .ok_or_else(|| anyhow::anyhow!("Invalid parameter tag format"))?;
                        let content_end = line
                            .find(&format!("</{}{}>", PARAM_TAG_PREFIX, current_param))
                            .ok_or_else(|| anyhow::anyhow!("Missing closing parameter tag"))?;

                        let value = line[content_start..content_end].trim().to_string();
                        debug!("Single-line parameter: {} = {}", current_param, value);
                        params.insert(current_param.clone(), value);
                        current_param.clear();
                    } else {
                        current_value.clear(); // Start collecting multi-line value
                    }
                }
                continue;
            }
        }

        // Check for parameter end with prefix
        if let Some(end_tag) = line.strip_prefix(&format!("</{}", PARAM_TAG_PREFIX)) {
            if end_tag
                .strip_suffix('>')
                .map_or(false, |name| name == current_param)
            {
                debug!("Parameter complete: {} = {}", current_param, current_value);
                params.insert(current_param.clone(), current_value.trim().to_string());
                current_param.clear();
                current_value.clear();
            }
            continue;
        }

        // If we're inside a parameter, collect its value
        if !current_param.is_empty() {
            if !current_value.is_empty() {
                current_value.push('\n');
            }
            current_value.push_str(line);
            debug!("Added value content: {}", current_value);
        }
    }

    debug!("Final parameters: {:?}", params);
    parse_tool_from_params(&tool_name, &params)
}

fn parse_tool_from_params(tool_name: &str, params: &HashMap<String, String>) -> Result<Tool> {
    match tool_name {
        "search_files" => Ok(Tool::SearchFiles {
            query: params
                .get("query")
                .ok_or_else(|| anyhow::anyhow!("Missing query"))?
                .to_string(),
            path: params.get("path").map(PathBuf::from),
            case_sensitive: params.get("case_sensitive").map_or(false, |v| v == "true"),
            whole_words: params.get("whole_words").map_or(false, |v| v == "true"),
            regex_mode: params.get("regex_mode").map_or(false, |v| v == "true"),
            max_results: params
                .get("max_results")
                .map(|v| v.trim().parse::<usize>())
                .transpose()?,
        }),

        "list_files" => Ok(Tool::ListFiles {
            paths: params
                .get("paths")
                .ok_or_else(|| anyhow::anyhow!("Missing paths"))?
                .split(',')
                .map(|s| PathBuf::from(s.trim()))
                .collect(),
            max_depth: params
                .get("max_depth")
                .map(|v| v.trim().parse::<usize>())
                .transpose()?,
        }),

        "read_files" => Ok(Tool::ReadFiles {
            paths: params
                .get("paths")
                .ok_or_else(|| anyhow::anyhow!("Missing paths"))?
                .split(',')
                .map(|s| PathBuf::from(s.trim()))
                .collect(),
        }),

        "summarize" => Ok(Tool::Summarize {
            files: params
                .get("files")
                .ok_or_else(|| anyhow::anyhow!("Missing files"))?
                .lines()
                .filter_map(|line| {
                    let mut parts = line.splitn(2, ':');
                    Some((
                        PathBuf::from(parts.next()?.trim()),
                        parts.next()?.trim().to_string(),
                    ))
                })
                .collect(),
        }),

        "update_file" => Ok(Tool::UpdateFile {
            path: PathBuf::from(
                params
                    .get("path")
                    .ok_or_else(|| anyhow::anyhow!("Missing path"))?,
            ),
            updates: params
                .get("updates")
                .ok_or_else(|| anyhow::anyhow!("Missing updates"))?
                .lines()
                .filter_map(|line| {
                    let mut parts = line.split(',');
                    Some(FileUpdate {
                        start_line: parts.next()?.trim().parse().ok()?,
                        end_line: parts.next()?.trim().parse().ok()?,
                        new_content: parts.next()?.trim().to_string(),
                    })
                })
                .collect(),
        }),

        "write_file" => Ok(Tool::WriteFile {
            path: PathBuf::from(
                params
                    .get("path")
                    .ok_or_else(|| anyhow::anyhow!("Missing path"))?,
            ),
            content: params
                .get("content")
                .ok_or_else(|| anyhow::anyhow!("Missing content"))?
                .to_string(),
        }),

        "delete_files" => Ok(Tool::DeleteFiles {
            paths: params
                .get("paths")
                .ok_or_else(|| anyhow::anyhow!("Missing paths"))?
                .split(',')
                .map(|s| PathBuf::from(s.trim()))
                .collect(),
        }),

        "ask_user" => Ok(Tool::AskUser {
            question: params
                .get("question")
                .ok_or_else(|| anyhow::anyhow!("Missing question"))?
                .to_string(),
        }),

        "message_user" => Ok(Tool::MessageUser {
            message: params
                .get("message")
                .ok_or_else(|| anyhow::anyhow!("Missing message"))?
                .to_string(),
        }),

        "complete_task" => Ok(Tool::CompleteTask {
            message: params
                .get("message")
                .ok_or_else(|| anyhow::anyhow!("Missing message"))?
                .to_string(),
        }),

        "execute_command" => Ok(Tool::ExecuteCommand {
            command_line: params
                .get("command_line")
                .ok_or_else(|| anyhow::anyhow!("Missing command_line"))?
                .to_string(),
            working_dir: params.get("working_dir").map(PathBuf::from),
        }),

        _ => Err(anyhow::anyhow!("Unknown tool: {}", tool_name)),
    }
}

fn parse_tool_json(name: &str, params: &serde_json::Value) -> Result<Tool> {
    match name {
        "execute_command" => Ok(Tool::ExecuteCommand {
            command_line: params["command_line"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing command_line"))?
                .to_string(),
            working_dir: params
                .get("working_dir")
                .and_then(|d| d.as_str())
                .map(PathBuf::from),
        }),
        "search_files" => Ok(Tool::SearchFiles {
            query: params["query"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing query"))?
                .to_string(),
            path: params
                .get("path")
                .and_then(|p| p.as_str())
                .map(PathBuf::from),
            case_sensitive: params
                .get("case_sensitive")
                .and_then(|b| b.as_bool())
                .unwrap_or(false),
            whole_words: params
                .get("whole_words")
                .and_then(|b| b.as_bool())
                .unwrap_or(false),
            regex_mode: params
                .get("mode")
                .and_then(|m| m.as_str())
                .map_or(false, |m| m == "regex"),
            max_results: params
                .get("max_results")
                .and_then(|n| n.as_u64())
                .map(|n| n as usize),
        }),
        "list_files" => Ok(Tool::ListFiles {
            paths: params["paths"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Missing or invalid paths array"))?
                .iter()
                .map(|p| {
                    Ok(PathBuf::from(
                        p.as_str()
                            .ok_or_else(|| anyhow::anyhow!("Invalid path in array"))?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?,
            max_depth: params["max_depth"].as_u64().map(|d| d as usize),
        }),
        "read_files" => Ok(Tool::ReadFiles {
            paths: params["paths"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Missing or invalid paths array"))?
                .iter()
                .map(|p| {
                    Ok(PathBuf::from(
                        p.as_str()
                            .ok_or_else(|| anyhow::anyhow!("Invalid path in array"))?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?,
        }),
        "summarize" => Ok(Tool::Summarize {
            files: params["files"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Missing or invalid files array"))?
                .iter()
                .map(|f| {
                    Ok((
                        PathBuf::from(
                            f["path"]
                                .as_str()
                                .ok_or_else(|| anyhow::anyhow!("Missing path in file entry"))?,
                        ),
                        f["summary"]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("Missing summary in file entry"))?
                            .to_string(),
                    ))
                })
                .collect::<Result<Vec<_>>>()?,
        }),
        "update_file" => Ok(Tool::UpdateFile {
            path: PathBuf::from(
                params["path"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing path parameter"))?,
            ),
            updates: params["updates"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Missing or invalid updates array"))?
                .iter()
                .map(|update| {
                    Ok(FileUpdate {
                        start_line: update["start_line"]
                            .as_u64()
                            .ok_or_else(|| anyhow::anyhow!("Invalid or missing start_line"))?
                            as usize,
                        end_line: update["end_line"]
                            .as_u64()
                            .ok_or_else(|| anyhow::anyhow!("Invalid or missing end_line"))?
                            as usize,
                        new_content: update["new_content"]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("Missing new_content"))?
                            .to_string(),
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        }),
        "write_file" => Ok(Tool::WriteFile {
            path: PathBuf::from(
                params["path"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing path parameter"))?,
            ),
            content: params["content"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing content parameter"))?
                .to_string(),
        }),
        "delete_files" => Ok(Tool::DeleteFiles {
            paths: params["paths"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Missing or invalid paths array"))?
                .iter()
                .map(|p| {
                    Ok(PathBuf::from(
                        p.as_str()
                            .ok_or_else(|| anyhow::anyhow!("Invalid path in array"))?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?,
        }),
        "ask_user" => Ok(Tool::AskUser {
            question: params["question"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing question parameter"))?
                .to_string(),
        }),
        "message_user" => Ok(Tool::MessageUser {
            message: params["message"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing message parameter"))?
                .to_string(),
        }),
        "complete_task" => Ok(Tool::CompleteTask {
            message: params["message"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing message parameter"))?
                .to_string(),
        }),
        _ => Err(anyhow::anyhow!("Unknown tool: {}", name)),
    }
}

fn update_tree_entry(
    tree: &mut FileTreeEntry,
    path: &PathBuf,
    new_entry: FileTreeEntry,
) -> Result<()> {
    let components: Vec<_> = path.components().collect();
    let mut current = tree;

    for (i, component) in components.iter().enumerate() {
        let name = component.as_os_str().to_string_lossy().to_string();
        let is_last = i == components.len() - 1;

        if is_last {
            current.children.insert(name, new_entry.clone());
            break;
        }

        current = current
            .children
            .get_mut(&name)
            .ok_or_else(|| anyhow::anyhow!("Path component not found: {}", name))?;
    }

    Ok(())
}
