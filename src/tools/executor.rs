use super::ToolResultHandler;
use crate::types::{ActionResult, CodeExplorer, SearchMode, SearchOptions, Tool, ToolResult};
use crate::ui::{UIMessage, UserInterface};
use crate::utils::CommandExecutor;
use anyhow::Result;
use std::collections::HashMap;

pub struct ToolExecutor {}

impl ToolExecutor {
    pub async fn execute<H: ToolResultHandler>(
        handler: &mut H,
        explorer: &Box<dyn CodeExplorer>,
        command_executor: &Box<dyn CommandExecutor>,
        ui: Option<&Box<dyn UserInterface>>,
        tool: &Tool,
        reasoning: &str,
    ) -> Result<(String, ToolResult)> {
        let action_result = match tool {
            Tool::ReadFiles { paths } => {
                let mut loaded_files = HashMap::new();
                let mut failed_files = Vec::new();

                for path in paths {
                    match explorer.read_file(path) {
                        Ok(content) => {
                            loaded_files.insert(path.clone(), content);
                        }
                        Err(e) => {
                            failed_files.push((path.clone(), e.to_string()));
                        }
                    }
                }

                ActionResult {
                    tool: tool.clone(),
                    result: ToolResult::ReadFiles {
                        loaded_files,
                        failed_files,
                    },
                    reasoning: reasoning.to_string(),
                }
            }

            Tool::ListFiles { paths, max_depth } => {
                let mut expanded_paths = Vec::new();
                let mut failed_paths = Vec::new();

                for path in paths {
                    match explorer.list_files(path, *max_depth) {
                        Ok(tree_entry) => {
                            expanded_paths.push((path.clone(), tree_entry));
                        }
                        Err(e) => {
                            failed_paths.push((path.display().to_string(), e.to_string()));
                        }
                    }
                }

                ActionResult {
                    tool: tool.clone(),
                    result: ToolResult::ListFiles {
                        expanded_paths,
                        failed_paths,
                    },
                    reasoning: reasoning.to_string(),
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

                let search_path = if let Some(p) = path {
                    p.clone()
                } else {
                    explorer.root_dir()
                };

                match explorer.search(&search_path, options) {
                    Ok(results) => ActionResult {
                        tool: tool.clone(),
                        result: ToolResult::SearchFiles {
                            results,
                            query: query.clone(),
                        },
                        reasoning: reasoning.to_string(),
                    },
                    Err(e) => ActionResult {
                        tool: tool.clone(),
                        result: ToolResult::SearchFiles {
                            results: Vec::new(),
                            query: format!("Search failed: {}", e),
                        },
                        reasoning: reasoning.to_string(),
                    },
                }
            }

            Tool::ExecuteCommand {
                command_line,
                working_dir,
            } => {
                match command_executor
                    .execute(command_line, working_dir.as_ref())
                    .await
                {
                    Ok(output) => ActionResult {
                        tool: tool.clone(),
                        result: ToolResult::ExecuteCommand {
                            success: output.success,
                            stdout: output.stdout,
                            stderr: output.stderr,
                        },
                        reasoning: reasoning.to_string(),
                    },
                    Err(e) => ActionResult {
                        tool: tool.clone(),
                        result: ToolResult::ExecuteCommand {
                            success: false,
                            stdout: String::new(),
                            stderr: e.to_string(),
                        },
                        reasoning: reasoning.to_string(),
                    },
                }
            }

            Tool::WriteFile { path, content } => match std::fs::write(path, content) {
                Ok(_) => ActionResult {
                    tool: tool.clone(),
                    result: ToolResult::WriteFile {
                        path: path.clone(),
                        success: true,
                    },
                    reasoning: reasoning.to_string(),
                },
                Err(_) => ActionResult {
                    tool: tool.clone(),
                    result: ToolResult::WriteFile {
                        path: path.clone(),
                        success: false,
                    },
                    reasoning: reasoning.to_string(),
                },
            },

            Tool::UpdateFile { path, updates } => match explorer.apply_updates(path, updates) {
                Ok(content) => ActionResult {
                    tool: tool.clone(),
                    result: ToolResult::UpdateFile {
                        path: path.clone(),
                        success: true,
                        content,
                    },
                    reasoning: reasoning.to_string(),
                },
                Err(e) => ActionResult {
                    tool: tool.clone(),
                    result: ToolResult::UpdateFile {
                        path: path.clone(),
                        success: false,
                        content: e.to_string(),
                    },
                    reasoning: reasoning.to_string(),
                },
            },

            Tool::Summarize { files } => ActionResult {
                tool: tool.clone(),
                result: ToolResult::Summarize {
                    files: files.clone(),
                },
                reasoning: reasoning.to_string(),
            },

            Tool::MessageUser { message } => match &ui {
                Some(ui) => match ui.display(UIMessage::Action(message.clone())).await {
                    Ok(_) => ActionResult {
                        tool: tool.clone(),
                        result: ToolResult::MessageUser {
                            result: "Message delivered".to_string(),
                        },
                        reasoning: reasoning.to_string(),
                    },
                    Err(e) => ActionResult {
                        tool: tool.clone(),
                        result: ToolResult::MessageUser {
                            result: format!("Failed to deliver message: {}", e),
                        },
                        reasoning: reasoning.to_string(),
                    },
                },
                None => ActionResult {
                    tool: tool.clone(),
                    result: ToolResult::MessageUser {
                        result: "Messaging user not available".to_string(),
                    },
                    reasoning: reasoning.to_string(),
                },
            },

            Tool::AskUser { question } => match &ui {
                Some(ui) => {
                    // Display the question
                    ui.display(UIMessage::Question(question.clone())).await?;

                    // Get the input
                    match ui.get_input("> ").await {
                        Ok(response) => ActionResult {
                            tool: tool.clone(),
                            result: ToolResult::AskUser { response },
                            reasoning: reasoning.to_string(),
                        },
                        Err(e) => ActionResult {
                            tool: tool.clone(),
                            result: ToolResult::AskUser {
                                response: format!("Failed to get user input: {}", e),
                            },
                            reasoning: reasoning.to_string(),
                        },
                    }
                }
                None => ActionResult {
                    tool: tool.clone(),
                    result: ToolResult::AskUser {
                        response: "User input not available".to_string(),
                    },
                    reasoning: reasoning.to_string(),
                },
            },

            Tool::CompleteTask { message } => match &ui {
                Some(ui) => match ui.display(UIMessage::Action(message.clone())).await {
                    Ok(_) => ActionResult {
                        tool: tool.clone(),
                        result: ToolResult::CompleteTask {
                            result: "Message delivered".to_string(),
                        },
                        reasoning: reasoning.to_string(),
                    },
                    Err(e) => ActionResult {
                        tool: tool.clone(),
                        result: ToolResult::CompleteTask {
                            result: format!("Failed to deliver message: {}", e),
                        },
                        reasoning: reasoning.to_string(),
                    },
                },
                None => ActionResult {
                    tool: tool.clone(),
                    result: ToolResult::CompleteTask {
                        result: "Messaging user not available".to_string(),
                    },
                    reasoning: reasoning.to_string(),
                },
            },

            Tool::DeleteFiles { paths } => {
                let mut deleted = Vec::new();
                let mut failed = Vec::new();

                for path in paths {
                    match std::fs::remove_file(path) {
                        Ok(_) => deleted.push(path.clone()),
                        Err(e) => failed.push((path.clone(), e.to_string())),
                    }
                }

                ActionResult {
                    tool: tool.clone(),
                    result: ToolResult::DeleteFiles { deleted, failed },
                    reasoning: reasoning.to_string(),
                }
            }
        };

        // Let the handler process the result and get formatted output
        let output = handler.handle_result(&action_result).await?;
        Ok((output, action_result.result))
    }
}
