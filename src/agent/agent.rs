use crate::llm::{ContentBlock, LLMProvider, LLMRequest, Message, MessageContent, MessageRole};
use crate::persistence::StatePersistence;
use crate::tool_definitions::Tools;
use crate::types::*;
use crate::ui::{UIMessage, UserInterface};
use crate::utils::{format_with_line_numbers, CommandExecutor};
use anyhow::Result;
use std::path::PathBuf;
use tracing::{debug, trace, warn};

pub struct Agent {
    working_memory: WorkingMemory,
    llm_provider: Box<dyn LLMProvider>,
    explorer: Box<dyn CodeExplorer>,
    command_executor: Box<dyn CommandExecutor>,
    ui: Box<dyn UserInterface>,
    state_persistence: Box<dyn StatePersistence>,
}

impl Agent {
    pub fn new(
        llm_provider: Box<dyn LLMProvider>,
        explorer: Box<dyn CodeExplorer>,
        command_executor: Box<dyn CommandExecutor>,
        ui: Box<dyn UserInterface>,
        state_persistence: Box<dyn StatePersistence>,
    ) -> Self {
        Self {
            working_memory: WorkingMemory::default(),
            llm_provider,
            explorer,
            ui,
            command_executor,
            state_persistence,
        }
    }

    async fn run_agent_loop(&mut self) -> Result<()> {
        // Main agent loop
        loop {
            let action = self.get_next_action().await?;

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
                break;
            }
        }

        debug!("Task completed");
        Ok(())
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

    /// Get next action from LLM
    async fn get_next_action(&self) -> Result<AgentAction> {
        let messages = self.prepare_messages();

        let request = LLMRequest {
            messages,
            max_tokens: 8192,
            temperature: 0.7,
            system_prompt: Some(format!(
                "You are an agent assisting the user in programming tasks. Your task is to analyze codebases and complete specific tasks.\n\n\
                Your goal is to either gather relevant information in the working memory, \
                or complete the task(s) if you have all necessary information.\n\
                \n\
                Working Memory Management:\n\
                - All path parameters are expected relative to the root directory\n\
                - Use list-files to expand collapsed directories (marked with ' [...]') in the repository structure\n\
                - Use read-files to load important files into working memory\n\
                - Use summarize to remove files that turned out to be less relevant\n\
                - Keep only information that's necessary for the current task\n\
                - Files that have been changed using update-file will always reflect the newest changes\n\
                \n\
                Before making changes to files, unless you already know the used libraries/dependencies,\n\
                always confirm that methods exist on the respective types by inspecting dependencies within the code-base!\n\
                \n\
                After making changes to code, always validate them using the execute-command tool with appropriate commands for the project type:\n\
                - For Rust projects: Use 'cargo check' and 'cargo test'\n\
                - For Node.js projects: Check package.json for test/lint scripts and use them\n\
                - For Python projects: Use pytest, mypy, or similar tools if available\n\
                - For other projects: Look for common build/test scripts and configuration files\n\
                \n\
                ALWAYS respond with your thoughts about what to do next first, then call the appropriate tool according to your reasoning.\n\
                Think step by step.",
            )),
            tools: Some(Tools::all()),
        };

        for (i, message) in request.messages.iter().enumerate() {
            if let MessageContent::Text(text) = &message.content {
                debug!("Message {}: Role={:?}\n---\n{}\n---", i, message.role, text);
            }
        }

        let response = self.llm_provider.send_message(request).await?;

        debug!("Raw LLM response:");
        for block in &response.content {
            if let ContentBlock::Text { text } = block {
                debug!("---\n{}\n---", text);
            }
            if let ContentBlock::ToolUse { id, name, input } = block {
                debug!("---\ntool: {}, input: {}\n---", name, input);
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

            Tool::Search {
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

// Helper function to parse LLM response into a Tool
fn parse_llm_response(response: &crate::llm::LLMResponse) -> Result<AgentAction> {
    // Extract the text content from the response
    let content = response
        .content
        .iter()
        .find_map(|block| {
            if let crate::llm::ContentBlock::Text { text } = block {
                Some(text.trim().trim_start_matches(|c| c != '{'))
            } else {
                None
            }
        })
        .ok_or_else(|| anyhow::anyhow!("No text content in response"))?;

    trace!("Raw JSON response: {}", content);

    // Escape newlines in the content, but only within strings
    let mut escaped = String::with_capacity(content.len());
    let mut in_string = false;
    let mut prev_char = None;

    for c in content.chars() {
        match c {
            '"' if prev_char != Some('\\') => {
                in_string = !in_string;
                escaped.push('"');
            }
            '\n' if in_string => escaped.push_str("\\n"),
            '\r' if in_string => escaped.push_str("\\r"),
            '\t' if in_string => escaped.push_str("\\t"),
            _ => escaped.push(c),
        }
        prev_char = Some(c);
    }

    trace!("Escaped JSON response: {}", escaped);

    // Parse the JSON response
    let value: serde_json::Value = serde_json::from_str(&escaped)
        .map_err(|e| anyhow::anyhow!("Failed to parse JSON response: {} JSON:\n{}", e, &escaped))?;

    // Extract the components
    let reasoning = value["reasoning"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing reasoning"))?
        .to_string();

    let tool_name = value["tool"]["name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing tool name"))?;

    let tool_params = &value["tool"]["params"];

    // Convert the tool JSON into our Tool enum
    let tool = match tool_name {
        "ListFiles" => Tool::ListFiles {
            paths: tool_params["paths"]
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
            max_depth: tool_params["max_depth"].as_u64().map(|d| d as usize),
        },
        "ReadFiles" => Tool::ReadFiles {
            paths: tool_params["paths"]
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
        },
        "WriteFile" => Tool::WriteFile {
            path: PathBuf::from(
                tool_params["path"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing path parameter"))?,
            ),
            content: tool_params["content"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing content parameter"))?
                .to_string(),
        },
        "UpdateFile" => Tool::UpdateFile {
            path: PathBuf::from(
                tool_params["path"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing path parameter"))?,
            ),
            updates: tool_params["updates"]
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
        },
        "Summarize" => Tool::Summarize {
            files: tool_params["files"]
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
        },
        "AskUser" => Tool::AskUser {
            question: tool_params["question"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing question parameter"))?
                .to_string(),
        },
        "MessageUser" => Tool::MessageUser {
            message: tool_params["message"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing message parameter"))?
                .to_string(),
        },
        "CompleteTask" => Tool::CompleteTask {
            message: tool_params["message"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing message parameter"))?
                .to_string(),
        },
        "ExecuteCommand" => Tool::ExecuteCommand {
            command_line: tool_params["command_line"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing command_line parameter"))?
                .to_string(),
            working_dir: tool_params["working_dir"].as_str().map(PathBuf::from),
        },
        "Search" => Tool::Search {
            query: tool_params["query"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing query parameter"))?
                .to_string(),
            path: tool_params["path"].as_str().map(PathBuf::from),
            case_sensitive: tool_params["case_sensitive"].as_bool().unwrap_or(false),
            whole_words: tool_params["whole_words"].as_bool().unwrap_or(false),
            regex_mode: tool_params["regex_mode"].as_bool().unwrap_or(false),
            max_results: tool_params["max_results"].as_u64().map(|n| n as usize),
        },
        _ => anyhow::bail!("Unknown tool: {}", tool_name),
    };

    debug!("Parsed agent action: tool={:?}", tool);
    debug!("Agent reasoning: {}", reasoning);

    Ok(AgentAction { tool, reasoning })
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
