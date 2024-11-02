use crate::explorer::CodeExplorer;
use crate::llm::{LLMProvider, LLMRequest, Message, MessageContent, MessageRole};
use crate::types::*;
use anyhow::Result;
use std::path::PathBuf;
use tracing::{debug, info, warn};

pub struct Agent {
    working_memory: WorkingMemory,
    llm_provider: Box<dyn LLMProvider>,
    explorer: CodeExplorer,
}

impl Agent {
    pub fn new(llm_provider: Box<dyn LLMProvider>, root_dir: PathBuf) -> Self {
        Self {
            working_memory: WorkingMemory::default(),
            llm_provider,
            explorer: CodeExplorer::new(root_dir),
        }
    }

    /// Start the agent with a specific task
    pub async fn start(&mut self, task: String) -> Result<()> {
        info!("Starting agent with task: {}", task);
        self.working_memory.current_task = task;

        // Main agent loop
        while !self.task_completed().await? {
            let action = self.get_next_action().await?;
            info!("Reasoning: {}", action.reasoning);
            info!("Executing next action: {:?}", action.tool);

            let result = self.execute_tool(action.tool).await?;
            self.working_memory.action_history.push(result);

            if action.task_completed {
                break;
            }

            self.manage_memory()?;
        }

        info!("Task completed");
        Ok(())
    }

    /// Get next action from LLM
    async fn get_next_action(&self) -> Result<AgentAction> {
        let messages = self.prepare_messages();

        let tools_description = r#"
    Available tools:
    1. ListFiles
       - Lists contents of a directory (non-recursive)
       - Parameters: {"path": "relative/or/absolute/path"}
       - Returns: List of files and directories with their types

    2. ReadFile
       - Reads the content of a file
       - Parameters: {"path": "path/to/file"}
       - Returns: Content of the file

    3. WriteFile
       - Creates or overwrites a file
       - Parameters: {
           "path": "path/to/file",
           "content": "content to write"
         }
       - Returns: Confirmation message

    4. UnloadFile
       - Removes a file from working memory to free up space
       - Parameters: {"path": "path/to/file"}
       - Returns: Confirmation message

    5. AddSummary
       - Adds or updates a file summary in working memory
       - Parameters: {
           "path": "path/to/file",
           "summary": "your summary of the file"
         }
       - Returns: Confirmation message"#;

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

        info!("Preparing LLM request with {} messages", messages.len());
        debug!(
            "System prompt: {}",
            request
                .system_prompt
                .as_ref()
                .unwrap_or(&"none".to_string())
        );

        let response = self.llm_provider.send_message(request).await?;

        info!("Received response from LLM");
        debug!("Raw LLM response: {:?}", response);

        parse_llm_response(&response)
    }

    /// Prepare messages for LLM request
    fn prepare_messages(&self) -> Vec<Message> {
        let mut messages = vec![Message {
            role: MessageRole::User,
            content: MessageContent::Text(format!(
                "Task: {}\n\nCurrent working memory state:\n{:?}",
                self.working_memory.current_task, self.working_memory
            )),
        }];

        // Add relevant history from previous actions
        for action in &self.working_memory.action_history {
            messages.push(Message {
                role: MessageRole::Assistant,
                content: MessageContent::Text(format!(
                    "Executed action: {:?}\nResult: {}",
                    action.tool, action.result
                )),
            });
        }

        messages
    }

    /// Executes a tool and returns the result
    async fn execute_tool(&mut self, tool: Tool) -> Result<ActionResult> {
        info!("Executing tool: {:?}", tool);

        let result = match &tool {
            Tool::ListFiles { path } => {
                let entries = self.explorer.list_directory(path)?;

                // Format the directory listing
                let mut listing = format!("Contents of {}:\n", path.display());
                for entry in entries {
                    let entry_type = match entry.entry_type {
                        FileSystemEntryType::File => "FILE",
                        FileSystemEntryType::Directory => "DIR ",
                    };
                    listing.push_str(&format!("{} {}\n", entry_type, entry.name));
                }

                ActionResult {
                    tool: tool.clone(),
                    success: true,
                    result: listing,
                    error: None,
                }
            }

            Tool::ReadFile { path } => {
                let full_path = if path.is_absolute() {
                    path.clone()
                } else {
                    self.explorer.root_dir.join(path)
                };

                match self.explorer.read_file(&full_path) {
                    Ok(content) => ActionResult {
                        tool: tool.clone(),
                        success: true,
                        result: content,
                        error: None,
                    },
                    Err(e) => ActionResult {
                        tool: tool.clone(),
                        success: false,
                        result: String::new(),
                        error: Some(e.to_string()),
                    },
                }
            }

            Tool::WriteFile { path, content } => {
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
                        tool: tool.clone(),
                        success: true,
                        result: format!("Successfully wrote to {}", full_path.display()),
                        error: None,
                    },
                    Err(e) => ActionResult {
                        tool: tool.clone(),
                        success: false,
                        result: String::new(),
                        error: Some(e.to_string()),
                    },
                }
            }

            Tool::UnloadFile { path } => {
                if self.working_memory.loaded_files.remove(path).is_some() {
                    ActionResult {
                        tool: tool.clone(),
                        success: true,
                        result: format!("Unloaded file {}", path.display()),
                        error: None,
                    }
                } else {
                    ActionResult {
                        tool: tool.clone(),
                        success: false,
                        result: String::new(),
                        error: Some(format!("File {} was not loaded", path.display())),
                    }
                }
            }

            Tool::AddSummary { path, summary } => {
                self.working_memory
                    .file_summaries
                    .insert(path.clone(), summary.clone());
                ActionResult {
                    tool: tool.clone(),
                    success: true,
                    result: format!("Added summary for {}", path.display()),
                    error: None,
                }
            }
        };

        // Log the result
        if result.success {
            info!("Tool execution successful: {:?}", tool);
        } else {
            warn!(
                "Tool execution failed: {:?}, error: {:?}",
                tool, result.error
            );
        }

        Ok(result)
    }

    /// Determines if the current task is completed
    async fn task_completed(&self) -> Result<bool> {
        // Implementation
        todo!()
    }

    /// Manages working memory size
    fn manage_memory(&mut self) -> Result<()> {
        // Implementation for memory management
        // Could implement strategies like:
        // - Removing old file contents
        // - Converting detailed content to summaries
        // - Limiting action history
        todo!()
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
                Some(text)
            } else {
                None
            }
        })
        .ok_or_else(|| anyhow::anyhow!("No text content in response"))?;

    // Parse the JSON response
    let value: serde_json::Value = serde_json::from_str(content)
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
        "ListFiles" => Tool::ListFiles {
            path: PathBuf::from(
                tool_params["path"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing path parameter"))?,
            ),
        },
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
        "UnloadFile" => Tool::UnloadFile {
            path: PathBuf::from(
                tool_params["path"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing path parameter"))?,
            ),
        },
        "AddSummary" => Tool::AddSummary {
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
        _ => anyhow::bail!("Unknown tool: {}", tool_name),
    };

    info!(
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
