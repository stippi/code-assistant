use crate::llm::{
    ContentBlock, LLMProvider, LLMRequest, Message, MessageContent, MessageRole, StreamingCallback,
};
use crate::persistence::StatePersistence;
use crate::tools::{
    parse_tool_json, parse_tool_xml, AgentToolHandler, ReplayToolHandler, ToolExecutor,
    TOOL_TAG_PREFIX,
};
use crate::types::*;
use crate::ui::{UIMessage, UserInterface};
use crate::utils::CommandExecutor;
use anyhow::Result;
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

            // Create fresh working memory for replay
            let mut replay_memory = WorkingMemory::default();
            replay_memory.current_task = state.task.clone();
            replay_memory.file_tree = Some(self.explorer.create_initial_tree(2)?);

            // Create replay executor
            let mut replay_handler = ReplayToolHandler::new(replay_memory);

            self.ui
                .display(UIMessage::Action(format!(
                    "Continuing task: {}, replaying {} actions",
                    state.task,
                    state.actions.len()
                )))
                .await?;

            // Replay actions into replay memory
            for original_action in state.actions {
                debug!("Replaying action: {:?}", original_action.tool);
                let action = AgentAction {
                    tool: original_action.tool.clone(),
                    reasoning: original_action.reasoning.clone(),
                };

                if let Ok((_, result)) = ToolExecutor::execute(
                    &mut replay_handler,
                    &self.explorer,
                    &self.command_executor,
                    Some(&self.ui),
                    &action.tool,
                )
                .await
                {
                    if result.is_success() {
                        self.working_memory.action_history.push(ActionResult {
                            tool: action.tool,
                            result,
                            reasoning: action.reasoning,
                        });
                    } else {
                        // On failure use original result
                        self.working_memory.action_history.push(original_action);
                    }
                } else {
                    // On error use original result
                    self.working_memory.action_history.push(original_action);
                }
            }

            // Take the replayed memory
            self.working_memory = replay_handler.into_memory();

            self.run_agent_loop().await
        } else {
            anyhow::bail!("No saved state found")
        }
    }

    /// Get next actions from LLM
    async fn get_next_actions(&self) -> Result<Vec<AgentAction>> {
        let messages = self.prepare_messages();

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
        debug!(
            "==== Token usage: Input: {}, Output: {}",
            response.usage.input_tokens, response.usage.output_tokens
        );

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
        memory.push_str("- Loaded files and their contents:\n");
        for (path, content) in &self.working_memory.loaded_files {
            memory.push_str(&format!("\n-----{}:\n{}\n", path.display(), content));
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
            memory.push_str(&format!("   Result: {}\n", action.result.format_message()));
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

        let mut handler = AgentToolHandler::new(&mut self.working_memory);

        // Execute the tool and get both the output and result
        let (output, tool_result) = ToolExecutor::execute(
            &mut handler,
            &self.explorer,
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
