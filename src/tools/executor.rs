use super::ToolResultHandler;
use crate::types::{CodeExplorer, SearchMode, SearchOptions, Tool, ToolResult};
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
    ) -> Result<(String, ToolResult)> {
        let result = match tool {
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

                ToolResult::ReadFiles {
                    loaded_files,
                    failed_files,
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

                ToolResult::ListFiles {
                    expanded_paths,
                    failed_paths,
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
                    Ok(results) => ToolResult::SearchFiles {
                        results,
                        query: query.clone(),
                    },
                    Err(e) => ToolResult::SearchFiles {
                        results: Vec::new(),
                        query: format!("Search failed: {}", e),
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
                    Ok(output) => ToolResult::ExecuteCommand {
                        success: output.success,
                        stdout: output.stdout,
                        stderr: output.stderr,
                    },
                    Err(e) => ToolResult::ExecuteCommand {
                        success: false,
                        stdout: String::new(),
                        stderr: e.to_string(),
                    },
                }
            }

            Tool::WriteFile { path, content } => {
                let full_path = if path.is_absolute() {
                    path.clone()
                } else {
                    explorer.root_dir().join(path)
                };

                // Ensure the parent directory exists
                if let Some(parent) = full_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }

                match std::fs::write(path, content) {
                    Ok(_) => ToolResult::WriteFile {
                        path: path.clone(),
                        success: true,
                        content: content.clone(),
                    },
                    Err(_) => ToolResult::WriteFile {
                        path: path.clone(),
                        success: false,
                        content: content.clone(),
                    },
                }
            }

            Tool::UpdateFile { path, updates } => match explorer.apply_updates(path, updates) {
                Ok(content) => ToolResult::UpdateFile {
                    path: path.clone(),
                    success: true,
                    content,
                },
                Err(e) => ToolResult::UpdateFile {
                    path: path.clone(),
                    success: false,
                    content: e.to_string(),
                },
            },

            Tool::Summarize { files } => ToolResult::Summarize {
                files: files.clone(),
            },

            Tool::MessageUser { message } => match &ui {
                Some(ui) => match ui.display(UIMessage::Action(message.clone())).await {
                    Ok(_) => ToolResult::MessageUser {
                        result: "Message delivered".to_string(),
                    },
                    Err(e) => ToolResult::MessageUser {
                        result: format!("Failed to deliver message: {}", e),
                    },
                },
                None => ToolResult::MessageUser {
                    result: "Messaging user not available".to_string(),
                },
            },

            Tool::AskUser { question } => match &ui {
                Some(ui) => {
                    // Display the question
                    ui.display(UIMessage::Question(question.clone())).await?;

                    // Get the input
                    match ui.get_input("> ").await {
                        Ok(response) => ToolResult::AskUser { response },
                        Err(e) => ToolResult::AskUser {
                            response: format!("Failed to get user input: {}", e),
                        },
                    }
                }
                None => ToolResult::AskUser {
                    response: "User input not available".to_string(),
                },
            },

            Tool::CompleteTask { message } => match &ui {
                Some(ui) => match ui.display(UIMessage::Action(message.clone())).await {
                    Ok(_) => ToolResult::CompleteTask {
                        result: "Message delivered".to_string(),
                    },
                    Err(e) => ToolResult::CompleteTask {
                        result: format!("Failed to deliver message: {}", e),
                    },
                },
                None => ToolResult::CompleteTask {
                    result: "Messaging user not available".to_string(),
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

                ToolResult::DeleteFiles { deleted, failed }
            }
        };

        // Let the handler process the result and get formatted output
        let output = handler.handle_result(&result).await?;
        Ok((output, result))
    }
}
