use crate::explorer::CodeExplorer;
use crate::llm::{LLMProvider, LLMRequest, Message, MessageContent, MessageRole};
use crate::types::*;
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{info, warn};

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

        // Initial context building
        let files = self.explorer.list_files()?;
        info!("Found {} files in repository", files.len());

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

        let request = LLMRequest {
            messages,
            max_tokens: 1000,
            temperature: 0.7,
            system_prompt: Some(
                "You are a code exploration agent. Analyze the current state and decide on the next action. \
                ALWAYS respond in the following JSON format:\
                {\
                  'reasoning': 'your step-by-step reasoning',\
                  'task_completed': true/false,\
                  'tool': {\
                    'name': 'ToolName',\
                    'params': { tool specific parameters }\
                  }\
                }".to_string()
            ),
        };

        let response = self.llm_provider.send_message(request).await?;
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
    async fn execute_tool(&self, tool: Tool) -> Result<ActionResult> {
        match tool {
            Tool::ListFiles { path } => {
                // Implementation
                todo!()
            }
            Tool::ReadFile { path } => {
                // Implementation
                todo!()
            }
            // Other tool implementations...
            _ => todo!(),
        }
    }

    /// Determines if the current task is completed
    async fn task_completed(&self) -> Result<bool> {
        // Implementation
        todo!()
    }

    /// Returns descriptions of available tools
    fn get_tool_descriptions(&self) -> Vec<ToolDescription> {
        vec![
            ToolDescription {
                name: "ListFiles".to_string(),
                description: "Lists all files in a specified directory".to_string(),
                parameters: {
                    let mut map = HashMap::new();
                    map.insert(
                        "path".to_string(),
                        "Path to the directory to list".to_string(),
                    );
                    map
                },
            },
            // Other tool descriptions...
        ]
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

    Ok(AgentAction {
        tool,
        reasoning,
        task_completed,
    })
}
