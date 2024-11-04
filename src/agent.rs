use crate::explorer::CodeExplorer;
use crate::llm::{LLMProvider, LLMRequest, Message, MessageContent, MessageRole};
use crate::types::*;
use crate::ui::{UIMessage, UserInterface};
use anyhow::Result;
use std::path::PathBuf;
use tracing::{debug, info, trace, warn};

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
           - Reads the content of one or multiple files
           - Parameters: {"paths": ["path/to/file1", "path/to/file2", ...]}
           - Returns: Confirmation of which files were loaded into working memory

        2. WriteFile
           - Creates or overwrites a file
           - Parameters: {
               "path": "path/to/file",
               "content": "content to write"
             }
           - Returns: Confirmation message

        3. Summarize
           - Replaces file contents with summaries in working memory
           - Parameters: {
               "files": [
                   {"path": "path/to/file1", "summary": "your summary of the file1"},
                   {"path": "path/to/file2", "summary": "your summary of the file2"}
               ]
             }
           - Returns: Confirmation message
           - Use this to maintain a high-level understanding while managing memory usage

        4. AskUser
           - Asks the user a question and waits for their response
           - Parameters: {"question": "your question here?"}
           - Returns: The user's response as a string
           - Use this when you need clarification or a decision from the user

        5. MessageUser
           - Provide a message to the user
           - Parameters: {"message": "your message here"}
           - Returns: Confirmation message
           - Use this when you need to inform the user"#;

        let request = LLMRequest {
            messages,
            max_tokens: 1000,
            temperature: 0.7,
            system_prompt: Some(format!(
                "You are an agent assisting the user in programming tasks. Your task is to analyze codebases and complete specific tasks.\n\n\
                Your goal is to either gather relevant information in the working memory, \
                or complete the task(s) if you have all necessary information.\n\n\
                Working Memory Management:\n\
                - Use ReadFile to load important files into working memory\n\
                - Use Summarize to remove files that turned out to be less relevant\n\
                - Keep only information that's necessary for the current task\n\
                - Use WriteFile to create new files or replace existing files. Always provide the complete content when writing files\n\n\
                {}\n\n\
                ALWAYS respond in the following JSON format:\n\
                {{\
                    \"reasoning\": <explain your thought process>,\
                    \"task_completed\": <true/false>,\
                    \"tool\": {{\
                        \"name\": <ToolName>,\
                        \"params\": <tool-specific parameters>\
                    }}\
                }}\n\n\
                Always explain your reasoning before choosing a tool. Think step by step.",
                tools_description
            )),
        };

        for (i, message) in request.messages.iter().enumerate() {
            info!(
                "Message {}: Role={:?}, Content={:?}",
                i, message.role, message.content
            );
        }

        let response = self.llm_provider.send_message(request).await?;

        debug!("Raw LLM response: {:?}", response);

        parse_llm_response(&response)
    }

    /// Prepare messages for LLM request - currently returns a single user message
    /// but kept as Vec<Message> for flexibility to change the format later
    fn prepare_messages(&self) -> Vec<Message> {
        vec![Message {
            role: MessageRole::User,
            content: MessageContent::Text(format!(
                "Task: {}\n\n\
                Repository structure:\n\
                {}\n\n\
                Current Working Memory:\n\
                - Loaded files and their contents:\n{}\n\
                - File summaries:\n{}\n\n\
                Previous actions:\n{}\n",
                self.working_memory.current_task,
                // File tree structure
                self.working_memory
                    .file_tree
                    .as_ref()
                    .map(|tree| tree.to_string())
                    .unwrap_or_else(|| "No file tree available".to_string()),
                // Format loaded files with their contents
                self.working_memory
                    .loaded_files
                    .iter()
                    .map(|(path, content)| format!("  -----{}:\n{}", path.display(), content))
                    .collect::<Vec<_>>()
                    .join("\n"),
                // Format file summaries
                self.working_memory
                    .file_summaries
                    .iter()
                    .map(|(path, summary)| format!("  {}: {}", path.display(), summary))
                    .collect::<Vec<_>>()
                    .join("\n"),
                // Format action history
                self.working_memory
                    .action_history
                    .iter()
                    .enumerate()
                    .map(|(i, action)| format!(
                        "{}. Tool: {:?}\n   Reasoning: {}\n   Result: {}",
                        i + 1,
                        action.tool,
                        action.reasoning,
                        action.result
                    ))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            )),
        }]
    }

    /// Executes an action and returns the result
    async fn execute_action(&mut self, action: &AgentAction) -> Result<ActionResult> {
        debug!("Executing action: {:?}", action.tool);

        let result = match &action.tool {
            Tool::ReadFile { paths } => {
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
                        self.explorer.root_dir.join(path)
                    };

                    match self.explorer.read_file(&full_path) {
                        Ok(content) => {
                            self.working_memory
                                .loaded_files
                                .insert(full_path.clone(), content);
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
                    String::new()
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
