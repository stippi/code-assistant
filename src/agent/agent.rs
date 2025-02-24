use crate::llm::{
    ContentBlock, LLMProvider, LLMRequest, Message, MessageContent, MessageRole, StreamingCallback,
};
use crate::persistence::StatePersistence;
use crate::tools::{
    parse_tool_json, parse_tool_xml, AgentToolHandler, ToolExecutor, TOOL_TAG_PREFIX,
};
use crate::types::*;
use crate::ui::{UIMessage, UserInterface};
use crate::utils::CommandExecutor;
use anyhow::Result;
use percent_encoding;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::debug;

const SYSTEM_MESSAGE: &str = include_str!("../../resources/system_message.md");
const SYSTEM_MESSAGE_TOOLS: &str = include_str!("../../resources/system_message_tools.md");

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
                Tool::ReadFiles { paths } => {
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

        let ui = Arc::clone(&self.ui);
        let streaming_callback: StreamingCallback = Box::new(move |text: &str| {
            ui.display_streaming(text)
                .map_err(|e| anyhow::anyhow!("Failed to display streaming output: {}", e))
        });

        let response = self
            .llm_provider
            .send_message(request, Some(&streaming_callback))
            .await?;

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
            "\n==== Token usage: Input: {}, Output: {} ====",
            response.usage.input_tokens, response.usage.output_tokens
        );

        let assistant_msg = Message {
            role: MessageRole::Assistant,
            content: MessageContent::Structured(response.content.clone()),
        };

        match parse_llm_response(&response) {
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

        // Display any tool output to the user
        if !output.is_empty() {
            self.ui.display(UIMessage::Action(output)).await?;
        }

        Ok(ActionResult {
            tool: action.tool.clone(),
            result: tool_result,
            reasoning: action.reasoning.clone(),
        })
    }
}

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
                            reasoning: remove_thinking_tags(reasoning.trim()).to_owned(),
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
                reasoning: remove_thinking_tags(reasoning.trim()).to_owned(),
            });
            reasoning = String::new();
        }
    }

    Ok(actions)
}

fn remove_thinking_tags(input: &str) -> &str {
    if input.starts_with("<thinking>") && input.ends_with("</thinking>") {
        &input[10..input.len() - 11]
    } else {
        input
    }
}
