use crate::llm::{
    ContentBlock, LLMProvider, LLMRequest, Message, MessageContent, MessageRole, StreamingCallback,
    StreamingChunk,
};
use crate::persistence::StatePersistence;
use crate::tools::{
    parse_tool_json, parse_tool_xml, AgentToolHandler, ToolExecutor, TOOL_TAG_PREFIX,
};
use crate::types::*;
use crate::ui::{streaming::StreamProcessor, UIMessage, UserInterface};
use crate::utils::CommandExecutor;
use anyhow::Result;
use percent_encoding;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::debug;

use super::ToolMode;

const SYSTEM_MESSAGE: &str = include_str!("../../resources/working_memory/system_message.md");
const SYSTEM_MESSAGE_TOOLS: &str =
    include_str!("../../resources/working_memory/system_message_tools.md");

pub struct Agent {
    working_memory: WorkingMemory,
    llm_provider: Box<dyn LLMProvider>,
    tool_mode: ToolMode,
    explorer: Box<dyn CodeExplorer>,
    command_executor: Box<dyn CommandExecutor>,
    ui: Arc<Box<dyn UserInterface>>,
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
            ui: Arc::new(ui),
            command_executor,
            state_persistence,
        }
    }

    pub async fn get_input_from_ui(&self, prompt: &str) -> Result<String> {
        self.ui.get_input(prompt).await.map_err(|e| e.into())
    }

    async fn run_agent_loop(&mut self) -> Result<()> {
        // Main agent loop
        loop {
            // Start with just the working memory message
            let mut messages = self.prepare_messages();

            // Keep trying until all actions succeed
            let mut all_actions_succeeded = false;
            while !all_actions_succeeded {
                let (actions, assistant_msg) = match self.get_next_actions(messages.clone()).await {
                    Ok(result) => result,
                    Err(e) => match e {
                        AgentError::LLMError(e) => return Err(e),
                        AgentError::ActionError { error, message } => {
                            messages.push(message);

                            if let Some(tool_error) = error.downcast_ref::<ToolError>() {
                                match tool_error {
                                    ToolError::UnknownTool(t) => {
                                        messages.push(Message {
                                            role: MessageRole::User,
                                            content: MessageContent::Text(format!(
                                                "Unknown tool '{}'. Please use only available tools.",
                                                t
                                            )),
                                        });
                                        continue;
                                    }
                                    ToolError::ParseError(msg) => {
                                        messages.push(Message {
                                            role: MessageRole::User,
                                            content: MessageContent::Text(format!(
                                                "Tool parameter error: {}. Please try again.",
                                                msg
                                            )),
                                        });
                                        continue;
                                    }
                                }
                            }
                            return Err(error);
                        }
                    },
                };
                messages.push(assistant_msg);

                // If no actions were returned, get user input
                if actions.is_empty() {
                    // Get input from UI
                    let user_input = self.get_input_from_ui("").await?;

                    // Display the user input as a user message in the UI
                    self.ui
                        .display(UIMessage::UserInput(user_input.clone()))
                        .await?;

                    // Add user input as an action result to working memory
                    let action_result = ActionResult {
                        tool: Tool::UserInput {},
                        result: ToolResult::UserInput {
                            message: user_input,
                        },
                        reasoning: "User provided input".to_string(),
                    };

                    self.working_memory.action_history.push(action_result);

                    // Save state after user input
                    self.state_persistence.save_state(
                        self.working_memory.current_task.clone(),
                        self.working_memory.action_history.clone(),
                    )?;

                    // Notify UI of working memory change
                    let _ = self.ui.update_memory(&self.working_memory).await;

                    // Break the inner loop to start a new iteration with updated working memory
                    break;
                }

                all_actions_succeeded = true; // Will be set to false if any action fails

                for action in actions {
                    let result = self.execute_action(&action).await?;

                    if !result.result.is_success() {
                        all_actions_succeeded = false;
                        // Add error message to conversation
                        messages.push(Message {
                            role: MessageRole::User,
                            content: MessageContent::Text(format!(
                                "Error executing action: {}\n{}",
                                result.reasoning,
                                result.result.format_message()
                            )),
                        });
                        break; // Stop processing remaining actions
                    }

                    self.working_memory.action_history.push(result);

                    // Save state after each successful action
                    self.state_persistence.save_state(
                        self.working_memory.current_task.clone(),
                        self.working_memory.action_history.clone(),
                    )?;

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

    /// Start a new agent task
    pub async fn start_with_task(&mut self, task: String) -> Result<()> {
        debug!("Starting agent with task: {}", task);
        self.working_memory.current_task = task.clone();

        self.ui.display(UIMessage::UserInput(task.clone())).await?;

        self.working_memory.file_tree = Some(self.explorer.create_initial_tree(2)?);

        // Save initial state
        self.state_persistence
            .save_state(task, self.working_memory.action_history.clone())?;

        // Notify UI of initial working memory
        let _ = self.ui.update_memory(&self.working_memory).await;

        self.run_agent_loop().await
    }

    /// Continue from a saved state
    pub async fn start_from_state(&mut self) -> Result<()> {
        if let Some(state) = self.state_persistence.load_state()? {
            debug!("Continuing task: {}", state.task);

            // Initialize working memory
            self.working_memory.current_task = state.task.clone();
            self.working_memory.file_tree = Some(self.explorer.create_initial_tree(2)?);

            // Restore action history from saved state
            self.working_memory.action_history = state.actions.clone();

            // Load current state of files into memory
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
        // Collect all file paths that should currently exist
        let mut existing_files = std::collections::HashSet::new();
        let root_dir = self.explorer.root_dir();

        // First pass: Handle files
        for action in &self.working_memory.action_history {
            match &action.tool {
                Tool::WriteFile { path, .. } | Tool::ReplaceInFile { path, .. } => {
                    // Convert relative to absolute path
                    let abs_path = if path.is_absolute() {
                        path.clone()
                    } else {
                        root_dir.join(path)
                    };
                    existing_files.insert(abs_path);
                }
                Tool::ReadFiles { paths, .. } => {
                    for path in paths {
                        // Convert relative to absolute path
                        let abs_path = if path.is_absolute() {
                            path.clone()
                        } else {
                            root_dir.join(path)
                        };
                        existing_files.insert(abs_path);
                    }
                }
                Tool::DeleteFiles { paths } => {
                    for path in paths {
                        // Convert relative to absolute path
                        let abs_path = if path.is_absolute() {
                            path.clone()
                        } else {
                            root_dir.join(path)
                        };
                        existing_files.remove(&abs_path);
                    }
                }
                _ => {}
            }
        }

        // Load all existing files into working memory
        for path in existing_files {
            if let Ok(content) = self.explorer.read_file(&path) {
                debug!("Loading existing file: {}", path.display());
                self.working_memory
                    .add_resource(path, LoadedResource::File(content));
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
                    self.working_memory.loaded_resources.insert(
                        path,
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
                    self.working_memory
                        .loaded_resources
                        .insert(path, LoadedResource::WebPage(page.clone()));
                }
                _ => {}
            }
        }

        Ok(())
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
            system_prompt: match self.tool_mode {
                ToolMode::Native => SYSTEM_MESSAGE.to_string(),
                ToolMode::Xml => SYSTEM_MESSAGE_TOOLS.to_string(),
            },
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
        let processor = Arc::new(Mutex::new(StreamProcessor::new(ui)));

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

    /// Prepare messages for LLM request - currently returns a single user message
    /// but kept as Vec<Message> for flexibility to change the format later
    fn prepare_messages(&self) -> Vec<Message> {
        vec![Message {
            role: MessageRole::User,
            content: MessageContent::Text(self.working_memory.to_markdown()),
        }]
    }

    /// Executes an action and returns the result
    async fn execute_action(&mut self, action: &AgentAction) -> Result<ActionResult> {
        debug!("Executing action: {:?}", action.tool);

        // Update status to Running before execution
        self.ui
            .update_tool_status(&action.tool_id, crate::ui::ToolStatus::Running, None)
            .await?;

        let mut handler = AgentToolHandler::new(&mut self.working_memory);

        // Execute the tool and get both the output and result
        let (output, tool_result) = ToolExecutor::execute(
            &mut handler,
            Some(&mut self.explorer),
            &self.command_executor,
            Some(&self.ui),
            &action.tool,
        )
        .await?;

        // Determine status based on result
        let status = if tool_result.is_success() {
            crate::ui::ToolStatus::Success
        } else {
            crate::ui::ToolStatus::Error
        };

        // Update tool status with result
        self.ui
            .update_tool_status(&action.tool_id, status, Some(output))
            .await?;

        Ok(ActionResult {
            tool: action.tool.clone(),
            result: tool_result,
            reasoning: action.reasoning.clone(),
        })
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
