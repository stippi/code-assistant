use super::ToolResultHandler;
use crate::config;
use crate::types::{CodeExplorer, SearchMode, SearchOptions, Tool, ToolResult};
use crate::ui::{UIMessage, UserInterface};
use crate::utils::CommandExecutor;
use crate::web::{WebClient, WebPage};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

pub struct ToolExecutor {}

fn check_absolute_path(path: &Path) -> Option<ToolResult> {
    if path.is_absolute() {
        Some(ToolResult::AbsolutePathError {
            path: path.to_path_buf(),
        })
    } else {
        None
    }
}

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

            Tool::UpdatePlan { plan } => ToolResult::UpdatePlan { plan: plan.clone() },

            Tool::Summarize { resources } => ToolResult::Summarize {
                resources: resources.clone(),
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

            Tool::WebSearch {
                query,
                hits_page_number,
            } => {
                // Create new client for each request
                let client = WebClient::new().await?;
                match client.search(query, *hits_page_number).await {
                    Ok(results) => ToolResult::WebSearch {
                        query: query.to_string(),
                        results,
                        error: None,
                    },
                    Err(e) => ToolResult::WebSearch {
                        query: query.to_string(),
                        results: vec![],
                        error: Some(e.to_string()),
                    },
                }
            }

            Tool::WebFetch { url, selectors: _ } => {
                // Create new client for each request
                let client = WebClient::new().await?;
                match client.fetch(url).await {
                    Ok(page) => ToolResult::WebFetch { page, error: None },
                    Err(e) => ToolResult::WebFetch {
                        page: WebPage::default(),
                        error: Some(e.to_string()),
                    },
                }
            }

            _ => {
                let explorer = explorer.ok_or_else(|| {
                    anyhow::anyhow!("This tool requires an active project. Use open_project first.")
                })?;
                match tool {
                    Tool::ReadFiles { paths } => {
                        // Check for absolute paths
                        for path in paths {
                            if let Some(error) = check_absolute_path(path) {
                                return Ok((String::new(), error));
                            }
                        }
                        let mut loaded_files = HashMap::new();
                        let mut failed_files = Vec::new();

                        for path in paths {
                            let full_path = explorer.root_dir().join(path);
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
                        // Check for absolute paths
                        for path in paths {
                            if let Some(error) = check_absolute_path(path) {
                                return Ok((String::new(), error));
                            }
                        }
                        let explorer = explorer; // Shadow with non-ref binding
                        let mut expanded_paths = Vec::new();
                        let mut failed_paths = Vec::new();

                        for path in paths {
                            let full_path = explorer.root_dir().join(path);
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

                    Tool::SearchFiles { regex } => {
                        let options = SearchOptions {
                            query: regex.clone(),
                            case_sensitive: false,
                            whole_words: false,
                            mode: SearchMode::Regex,
                            max_results: None,
                        };

                        let search_path = explorer.root_dir();

                        match explorer.search(&search_path, options) {
                            Ok(mut results) => {
                                // Convert absolute paths to relative paths
                                let root_dir = explorer.root_dir();
                                for result in &mut results {
                                    if let Ok(rel_path) = result.file.strip_prefix(&root_dir) {
                                        result.file = rel_path.to_path_buf();
                                    }
                                }

                                ToolResult::SearchFiles {
                                    results,
                                    regex: regex.clone(),
                                }
                            }
                            Err(e) => ToolResult::SearchFiles {
                                results: Vec::new(),
                                regex: format!("Search failed: {}", e),
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
                                        output:
                                            "Working directory must be relative to project root"
                                                .to_string(),
                                        success: false,
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
                                output: output.output,
                                success: output.success,
                            },
                            Err(e) => ToolResult::ExecuteCommand {
                                output: e.to_string(),
                                success: false,
                            },
                        }
                    }

                    Tool::WriteFile {
                        path,
                        content,
                        append,
                    } => {
                        if let Some(error) = check_absolute_path(path) {
                            return Ok((String::new(), error));
                        }
                        let full_path = explorer.root_dir().join(path);

                        match explorer.write_file(&full_path, content, *append) {
                            Ok(_) => ToolResult::WriteFile {
                                path: path.clone(),
                                content: content.clone(),
                                error: None,
                            },
                            Err(e) => ToolResult::WriteFile {
                                path: path.clone(),
                                content: String::new(), // Empty content on error
                                error: Some(e.to_string()),
                            },
                        }
                    }

                    Tool::ReplaceInFile { path, replacements } => {
                        if let Some(error) = check_absolute_path(path) {
                            return Ok((String::new(), error));
                        }
                        let full_path = explorer.root_dir().join(path);

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
                        // Check for absolute paths
                        for path in paths {
                            if let Some(error) = check_absolute_path(path) {
                                return Ok((String::new(), error));
                            }
                        }
                        let mut deleted = Vec::new();
                        let mut failed = Vec::new();

                        for path in paths {
                            let full_path = explorer.root_dir().join(path);
                            match explorer.delete_file(&full_path) {
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
