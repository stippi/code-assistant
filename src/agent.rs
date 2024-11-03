use crate::explorer::CodeExplorer;
use crate::llm::{LLMProvider, LLMRequest, Message, MessageContent, MessageRole};
use crate::types::*;
use crate::ui::{UIMessage, UserInterface};
use anyhow::Result;
use std::path::PathBuf;
use tracing::{debug, trace, warn};

pub struct Agent {
    working_memory: WorkingMemory,
    llm_provider: Box<dyn LLMProvider>,
    explorer: CodeExplorer,
    ui: Box<dyn UserInterface>,
}

impl Agent {
    pub fn new(
        llm_provider: Box<dyn LLMProvider>,
        root_dir: PathBuf,
        ui: Box<dyn UserInterface>,
    ) -> Self {
        Self {
            working_memory: WorkingMemory::default(),
            llm_provider,
            explorer: CodeExplorer::new(root_dir),
            ui,
        }
    }

    /// Start the agent with a specific task
    pub async fn start(&mut self, task: String) -> Result<()> {
        debug!("Starting agent with task: {}", task);
        self.working_memory.current_task = task;

        // Create initial file tree
        self.ui
            .display(UIMessage::Action(
                "Creating repository file tree...".to_string(),
            ))
            .await?;

        self.working_memory.file_tree = Some(self.explorer.create_file_tree()?);

        // Main agent loop
        loop {
            let action = self.get_next_action().await?;

            let result = self.execute_action(&action).await?;
            self.working_memory.action_history.push(result);

            if action.task_completed {
                break;
            }
        }

        debug!("Task completed");
        Ok(())
    }

    /// Get next action from LLM
    async fn get_next_action(&self) -> Result<AgentAction> {
        let messages = self.prepare_messages();

        let tools_description = r#"
    Available tools:
    1. ReadFile
       - Reads the content of a file
       - Parameters: {"path": "path/to/file"}
       - Returns: Content of the file

    2. WriteFile
       - Creates or overwrites a file
       - Parameters: {
           "path": "path/to/file",
           "content": "content to write"
         }
       - Returns: Confirmation message

    3. Summarize
       - Replaces file content with a summary in working memory
       - Parameters: {
           "path": "path/to/file",
           "summary": "your summary of the file",
         }
       - Returns: Confirmation message
       - Use this to maintain a high-level understanding while managing memory usage

    4. AskUser
       - Asks the user a question and waits for their response
       - Parameters: {"question": "your question here?"}
       - Returns: The user's response as a string
       - Use this when you need clarification or a decision from the user"#;

        let request = LLMRequest {
            messages,
            max_tokens: 1000,
            temperature: 0.7,
            system_prompt: Some(format!(
                "You are a code exploration agent. Your task is to analyze codebases and complete specific tasks.\n\n\
                {}\n\n\
                When exploring directories, remember:\n\
                - Directory listings are non-recursive and show files (FILE) and directories (DIR)\n\
                - You need to explicitly navigate into subdirectories to see their contents\n\
                - Always start with the root directory and navigate step by step\n\n\
                ALWAYS respond in the following JSON format:\n\
                {{\
                  'reasoning': 'your step-by-step reasoning',\
                  'task_completed': true/false,\
                  'tool': {{\
                    'name': 'ToolName',\
                    'params': {{ tool specific parameters }}\
                  }}\
                }}\n\n\
                Always explain your reasoning before choosing a tool. Think step by step.",
                tools_description
            )),
        };

        // debug!(
        //     "System prompt: {}",
        //     request
        //         .system_prompt
        //         .as_ref()
        //         .unwrap_or(&"none".to_string())
        // );
        for (i, message) in request.messages.iter().enumerate() {
            debug!(
                "Message {}: Role={:?}, Content={:?}",
                i, message.role, message.content
            );
        }

        let response = self.llm_provider.send_message(request).await?;

        debug!("Raw LLM response: {:?}", response);

        parse_llm_response(&response)
    }

    /// Prepare messages for LLM request
    fn prepare_messages(&self) -> Vec<Message> {
        let mut messages = vec![Message {
            role: MessageRole::User,
            content: MessageContent::Text(
                format!(
                    "Task: {}\n\nRepository structure:\n{}\n\nCurrent working memory state:\n- Loaded files: {}\n- File summaries: {}\n",
                    self.working_memory.current_task,
                    self.working_memory.file_tree.as_ref()
                        .map(|tree| tree.to_string())
                        .unwrap_or_else(|| "No file tree available".to_string()),
                    self.working_memory.loaded_files.keys()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                    self.working_memory.file_summaries.keys()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
                .trim()
                .to_string(),
            ),
        }];

        // Add relevant history from previous actions
        for action in &self.working_memory.action_history {
            messages.push(Message {
                role: MessageRole::Assistant,
                content: MessageContent::Text(
                    format!(
                        "Reasoning: {}\nExecuted action: {:?}\nResult: {}",
                        action.reasoning, action.tool, action.result
                    )
                    .trim()
                    .to_string(),
                ),
            });
        }

        messages
    }

    /// Executes an action and returns the result
    async fn execute_action(&mut self, action: &AgentAction) -> Result<ActionResult> {
        debug!("Executing action: {:?}", action.tool);

        let result = match &action.tool {
            Tool::ReadFile { path } => {
                self.ui
                    .display(UIMessage::Action(format!(
                        "Reading file `{}`",
                        path.display()
                    )))
                    .await?;

                let full_path = if path.is_absolute() {
                    path.clone()
                } else {
                    self.explorer.root_dir.join(path)
                };

                match self.explorer.read_file(&full_path) {
                    Ok(content) => ActionResult {
                        tool: action.tool.clone(),
                        success: true,
                        result: content,
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
                    self.explorer.root_dir.join(path)
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

            Tool::Summarize { path, summary } => {
                self.ui
                    .display(UIMessage::Action(format!(
                        "Summarizing file `{}`",
                        path.display()
                    )))
                    .await?;

                self.working_memory.loaded_files.remove(path);

                // Add the summary
                self.working_memory
                    .file_summaries
                    .insert(path.clone(), summary.clone());

                ActionResult {
                    tool: action.tool.clone(),
                    success: true,
                    result: format!("Summarized {} and updated working memory", path.display()),
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

    // Escape newlines in the content, but only withing strings
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
        .map_err(|e| anyhow::anyhow!("Failed to parse JSON response: {}", e))?;

    // Extract the components
    let reasoning = value["reasoning"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing reasoning"))?
        .to_string();

    let task_completed = value["task_completed"]
        .as_bool()
        .ok_or_else(|| anyhow::anyhow!("Missing task_completed"))?;

    let tool_name = value["tool"]["name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing tool name"))?;

    let tool_params = &value["tool"]["params"];

    // Convert the tool JSON into our Tool enum
    let tool = match tool_name {
        "ReadFile" => Tool::ReadFile {
            path: PathBuf::from(
                tool_params["path"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing path parameter"))?,
            ),
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
        "Summarize" => Tool::Summarize {
            path: PathBuf::from(
                tool_params["path"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing path parameter"))?,
            ),
            summary: tool_params["summary"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing summary parameter"))?
                .to_string(),
        },
        "AskUser" => Tool::AskUser {
            question: tool_params["question"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing question parameter"))?
                .to_string(),
        },
        _ => anyhow::bail!("Unknown tool: {}", tool_name),
    };

    debug!(
        "Parsed agent action: tool={:?}, task_completed={}",
        tool, task_completed
    );
    debug!("Agent reasoning: {}", reasoning);

    Ok(AgentAction {
        tool,
        reasoning,
        task_completed,
    })
}
