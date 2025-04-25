use super::ToolResultHandler;
use crate::config::{self, ProjectManager};
use crate::types::{SearchMode, SearchOptions, Tool, ToolResult};
use crate::ui::{UIMessage, UserInterface};
use crate::utils::CommandExecutor;
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use web::{WebClient, WebPage};

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
        project_manager: &Box<dyn ProjectManager>,
        command_executor: &Box<dyn CommandExecutor>,
        ui: Option<&Box<dyn UserInterface>>,
        tool: &Tool,
    ) -> Result<(String, ToolResult)> {
        let result = match tool {
            Tool::ListProjects => {
                let projects = config::load_projects()?;
                ToolResult::ListProjects { projects }
            }

            Tool::UpdatePlan { plan } => ToolResult::UpdatePlan { plan: plan.clone() },

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

            Tool::ReadFiles { project, paths } => {
                // Get explorer for the specified project
                let explorer = match project_manager.get_explorer_for_project(project) {
                    Ok(explorer) => explorer,
                    Err(e) => {
                        return Ok((
                            String::new(),
                            ToolResult::ReadFiles {
                                project: project.clone(),
                                loaded_files: HashMap::new(),
                                failed_files: vec![(
                                    PathBuf::from("."),
                                    format!(
                                        "Failed to get explorer for project {}: {}",
                                        project, e
                                    ),
                                )],
                            },
                        ));
                    }
                };

                // Check for absolute paths
                for path in paths {
                    if let Some(error) = check_absolute_path(path) {
                        return Ok((String::new(), error));
                    }
                }
                let mut loaded_files = HashMap::new();
                let mut failed_files = Vec::new();

                // Parse the path string to extract line range information
                use super::parse;

                for path in paths {
                    // Get the path string and parse it for line range information
                    let path_str = path.to_string_lossy();
                    let parsed_path = match parse::PathWithLineRange::parse(&path_str) {
                        Ok(parsed) => parsed,
                        Err(e) => {
                            failed_files.push((path.clone(), e.to_string()));
                            continue;
                        }
                    };

                    // Join with root_dir to get full path
                    let full_path = explorer.root_dir().join(&parsed_path.path);

                    // Use either read_file_range or read_file based on whether we have line range info
                    let read_result =
                        if parsed_path.start_line.is_some() || parsed_path.end_line.is_some() {
                            // We have line range information, use read_file_range
                            explorer.read_file_range(
                                &full_path,
                                parsed_path.start_line,
                                parsed_path.end_line,
                            )
                        } else {
                            // No line range specified, read the whole file
                            explorer.read_file(&full_path)
                        };

                    match read_result {
                        Ok(content) => {
                            loaded_files.insert(path.clone(), content);
                        }
                        Err(e) => {
                            failed_files.push((path.clone(), e.to_string()));
                        }
                    }
                }

                ToolResult::ReadFiles {
                    project: project.clone(),
                    loaded_files,
                    failed_files,
                }
            }

            Tool::ListFiles {
                project,
                paths,
                max_depth,
            } => {
                // Get explorer for the specified project
                let mut explorer = match project_manager.get_explorer_for_project(project) {
                    Ok(explorer) => explorer,
                    Err(e) => {
                        return Ok((
                            String::new(),
                            ToolResult::ListFiles {
                                project: project.clone(),
                                expanded_paths: Vec::new(),
                                failed_paths: vec![(
                                    format!("."),
                                    format!(
                                        "Failed to get explorer for project {}: {}",
                                        project, e
                                    ),
                                )],
                            },
                        ));
                    }
                };

                let mut expanded_paths = Vec::new();
                let mut failed_paths = Vec::new();

                for path in paths {
                    // Check if path is absolute and handle it properly
                    if path.is_absolute() {
                        failed_paths.push((
                            path.display().to_string(),
                            "Path must be relative to project root".to_string(),
                        ));
                        continue;
                    }

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
                    project: project.clone(),
                    expanded_paths,
                    failed_paths,
                }
            }

            Tool::SearchFiles { project, regex } => {
                // Get explorer for the specified project
                let explorer = match project_manager.get_explorer_for_project(project) {
                    Ok(explorer) => explorer,
                    Err(e) => {
                        return Ok((
                            String::new(),
                            ToolResult::SearchFiles {
                                project: project.clone(),
                                results: Vec::new(),
                                regex: format!(
                                    "Failed to get explorer for project {}: {}",
                                    project, e
                                ),
                            },
                        ));
                    }
                };

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
                            project: project.clone(),
                            results,
                            regex: regex.clone(),
                        }
                    }
                    Err(e) => ToolResult::SearchFiles {
                        project: project.clone(),
                        results: Vec::new(),
                        regex: format!("Search failed: {}", e),
                    },
                }
            }

            Tool::ExecuteCommand {
                project,
                command_line,
                working_dir,
            } => {
                // Get explorer for the specified project
                let explorer = match project_manager.get_explorer_for_project(project) {
                    Ok(explorer) => explorer,
                    Err(e) => {
                        return Ok((
                            String::new(),
                            ToolResult::ExecuteCommand {
                                project: project.clone(),
                                output: format!(
                                    "Failed to get explorer for project {}: {}",
                                    project, e
                                ),
                                success: false,
                            },
                        ));
                    }
                };

                let effective_working_dir = match working_dir {
                    Some(dir) if dir.is_absolute() => {
                        return Ok((
                            String::new(),
                            ToolResult::ExecuteCommand {
                                project: project.clone(),
                                output: "Working directory must be relative to project root"
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
                        project: project.clone(),
                        output: output.output,
                        success: output.success,
                    },
                    Err(e) => ToolResult::ExecuteCommand {
                        project: project.clone(),
                        output: e.to_string(),
                        success: false,
                    },
                }
            }

            Tool::WriteFile {
                project,
                path,
                content,
                append,
            } => {
                // Get explorer for the specified project
                let explorer = match project_manager.get_explorer_for_project(project) {
                    Ok(explorer) => explorer,
                    Err(e) => {
                        return Ok((
                            String::new(),
                            ToolResult::WriteFile {
                                project: project.clone(),
                                path: path.clone(),
                                content: String::new(),
                                error: Some(format!(
                                    "Failed to get explorer for project {}: {}",
                                    project, e
                                )),
                            },
                        ));
                    }
                };

                if let Some(error) = check_absolute_path(path) {
                    return Ok((String::new(), error));
                }
                let full_path = explorer.root_dir().join(path);

                match explorer.write_file(&full_path, content, *append) {
                    Ok(_) => ToolResult::WriteFile {
                        project: project.clone(),
                        path: path.clone(),
                        content: content.clone(),
                        error: None,
                    },
                    Err(e) => ToolResult::WriteFile {
                        project: project.clone(),
                        path: path.clone(),
                        content: String::new(), // Empty content on error
                        error: Some(e.to_string()),
                    },
                }
            }

            Tool::ReplaceInFile {
                project,
                path,
                replacements,
            } => {
                // Get explorer for the specified project
                let explorer = match project_manager.get_explorer_for_project(project) {
                    Ok(explorer) => explorer,
                    Err(e) => {
                        return Ok((
                            String::new(),
                            ToolResult::ReplaceInFile {
                                project: project.clone(),
                                path: path.clone(),
                                content: String::new(),
                                error: Some(crate::utils::FileUpdaterError::Other(format!(
                                    "Failed to get explorer for project {}: {}",
                                    project, e
                                ))),
                            },
                        ));
                    }
                };

                if let Some(error) = check_absolute_path(path) {
                    return Ok((String::new(), error));
                }
                let full_path = explorer.root_dir().join(path);

                // Read the current content in order to return it in error case
                let current_content = match std::fs::read_to_string(&full_path) {
                    Ok(content) => content,
                    Err(_) => String::new(),
                };

                match explorer.apply_replacements(&full_path, replacements) {
                    Ok(new_content) => ToolResult::ReplaceInFile {
                        project: project.clone(),
                        path: path.clone(),
                        content: new_content,
                        error: None,
                    },
                    Err(e) => {
                        // Extract FileUpdaterError if present or create Other variant
                        use crate::utils::FileUpdaterError;
                        let error = if let Some(file_err) = e.downcast_ref::<FileUpdaterError>() {
                            file_err.clone()
                        } else {
                            FileUpdaterError::Other(e.to_string())
                        };

                        ToolResult::ReplaceInFile {
                            project: project.clone(),
                            path: path.clone(),
                            content: current_content,
                            error: Some(error),
                        }
                    }
                }
            }

            Tool::DeleteFiles { project, paths } => {
                // Get explorer for the specified project
                let explorer = match project_manager.get_explorer_for_project(project) {
                    Ok(explorer) => explorer,
                    Err(e) => {
                        return Ok((
                            String::new(),
                            ToolResult::DeleteFiles {
                                project: project.clone(),
                                deleted: Vec::new(),
                                failed: vec![(
                                    PathBuf::from("."),
                                    format!(
                                        "Failed to get explorer for project {}: {}",
                                        project, e
                                    ),
                                )],
                            },
                        ));
                    }
                };

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

                ToolResult::DeleteFiles {
                    project: project.clone(),
                    deleted,
                    failed,
                }
            }

            Tool::PerplexityAsk { messages } => {
                // Check if the API key exists
                let api_key = match std::env::var("PERPLEXITY_API_KEY") {
                    Ok(key) => Some(key),
                    Err(_) => None,
                };

                // Create a new Perplexity client
                let client = web::PerplexityClient::new(api_key);

                // Extract last 'user' message for display
                let query = messages
                    .iter()
                    .filter(|m| m.role == "user")
                    .last()
                    .map(|m| m.content.clone())
                    .unwrap_or_else(|| "No user query found".to_string());

                match client.ask(&messages, None).await {
                    Ok(response) => ToolResult::PerplexityAsk {
                        query,
                        answer: response.content,
                        citations: response.citations,
                        error: None,
                    },
                    Err(e) => ToolResult::PerplexityAsk {
                        query,
                        answer: String::new(),
                        citations: vec![],
                        error: Some(e.to_string()),
                    },
                }
            }

            _ => unreachable!(),
        };

        // Let the handler process the result and get formatted output
        let output = handler.handle_result(&result).await?;
        Ok((output, result))
    }
}
