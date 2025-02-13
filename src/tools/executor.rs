use super::ToolResultHandler;
use crate::config;
use crate::types::{CodeExplorer, SearchMode, SearchOptions, Tool, ToolResult};
use crate::ui::{UIMessage, UserInterface};
use crate::utils::CommandExecutor;
use anyhow::Result;
use std::collections::HashMap;

pub struct ToolExecutor {}

impl ToolExecutor {
    pub async fn execute<H: ToolResultHandler>(
        handler: &mut H,
        explorer: Option<&mut Box<dyn CodeExplorer>>,
        command_executor: &Box<dyn CommandExecutor>,
        ui: Option<&Box<dyn UserInterface>>,
        tool: &Tool,
    ) -> Result<(String, ToolResult)> {
        let result = match tool {
            Tool::ListProjects => {
                let projects = config::load_projects()?;
                ToolResult::ListProjects { projects }
            }

            Tool::OpenProject { name } => {
                let projects = config::load_projects()?;
                match projects.get(name) {
                    Some(project) => ToolResult::OpenProject {
                        name: name.clone(),
                        path: Some(project.path.to_path_buf()),
                        error: None,
                    },
                    None => ToolResult::OpenProject {
                        name: name.clone(),
                        path: None,
                        error: Some("Project not found".to_string()),
                    },
                }
            }

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

            _ => {
                let explorer = explorer.ok_or_else(|| {
                    anyhow::anyhow!("This tool requires an active project. Use open_project first.")
                })?;
                match tool {
                    Tool::ReadFiles { paths } => {
                        let mut loaded_files = HashMap::new();
                        let mut failed_files = Vec::new();

                        for path in paths {
                            let full_path = if path.is_absolute() {
                                path.clone()
                            } else {
                                explorer.root_dir().join(path)
                            };
                            match explorer.read_file(&full_path) {
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
                        let explorer = explorer; // Shadow with non-ref binding
                        let mut expanded_paths = Vec::new();
                        let mut failed_paths = Vec::new();

                        for path in paths {
                            let full_path = if path.is_absolute() {
                                path.clone()
                            } else {
                                explorer.root_dir().join(path)
                            };
                            match explorer.list_files(&full_path, *max_depth) {
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
                            if p.is_absolute() {
                                p.clone()
                            } else {
                                explorer.root_dir().join(p)
                            }
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
                        let effective_working_dir = match working_dir {
                            Some(dir) if dir.is_absolute() => {
                                return Ok((
                                    String::new(),
                                    ToolResult::ExecuteCommand {
                                        stdout: String::new(),
                                        stderr: String::new(),
                                        error: Some("Working directory must be relative to project root".to_string()),
                                    },
                                ));
                            }
                            Some(dir) => explorer.root_dir().join(dir),
                            None => explorer.root_dir(),
                        };

                        match command_executor
                            .execute(command_line, Some(&effective_working_dir))
                            .await
                        {
                            Ok(output) => ToolResult::ExecuteCommand {
                                stdout: output.stdout,
                                stderr: output.stderr,
                                error: if output.success { None } else { Some("Command failed".to_string()) },
                            },
                            Err(e) => ToolResult::ExecuteCommand {
                                stdout: String::new(),
                                stderr: String::new(),
                                error: Some(e.to_string()),
                            },
                        }
                    }

                    Tool::WriteFile { path, content } => {
                        let full_path = if path.is_absolute() {
                            path.clone()
                        } else {
                            explorer.root_dir().join(path)
                        };

                        match explorer.write_file(&full_path, content) {
                            Ok(_) => ToolResult::WriteFile {
                                path: path.clone(),
                                content: content.clone(),
                                error: None,
                            },
                            Err(e) => ToolResult::WriteFile {
                                path: path.clone(),
                                content: String::new(),  // Empty content on error
                                error: Some(e.to_string()),
                            },
                        }
                    }

                    Tool::ReplaceInFile { path, replacements } => {
                        let full_path = if path.is_absolute() {
                            path.clone()
                        } else {
                            explorer.root_dir().join(path)
                        };

                        match explorer.apply_replacements(&full_path, replacements) {
                            Ok(new_content) => ToolResult::ReplaceInFile {
                                path: path.clone(),
                                content: new_content,
                                error: None,
                            },
                            Err(e) => ToolResult::ReplaceInFile {
                                path: path.clone(),
                                content: String::new(), // Empty content on error
                                error: Some(e.to_string()),
                            },
                        }
                    }

                    Tool::DeleteFiles { paths } => {
                        let mut deleted = Vec::new();
                        let mut failed = Vec::new();

                        for path in paths {
                            match explorer.delete_file(path) {
                                Ok(_) => deleted.push(path.clone()),
                                Err(e) => failed.push((path.clone(), e.to_string())),
                            }
                        }

                        ToolResult::DeleteFiles { deleted, failed }
                    }

                    _ => unreachable!(),
                }
            }
        };

        // Let the handler process the result and get formatted output
        let output = handler.handle_result(&result).await?;
        Ok((output, result))
    }
}
