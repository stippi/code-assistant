use crate::types::ToolResult;

impl ToolResult {
    // Format a user-facing message describing the result
    pub fn format_message(&self) -> String {
        match self {
            ToolResult::ListProjects { projects } => {
                if projects.is_empty() {
                    "No projects configured.".to_string()
                } else {
                    let mut msg = String::from("Available projects:\n");
                    for (name, project) in projects {
                        msg.push_str(&format!("- {}: {}\n", name, project.path.display()));
                    }
                    msg
                }
            }
            ToolResult::OpenProject { name, error, .. } => {
                if error.is_none() {
                    format!("Successfully opened project '{}'", name)
                } else {
                    format!(
                        "Failed to open project '{}': {}",
                        name,
                        error.as_ref().unwrap_or(&"unknown error".to_string())
                    )
                }
            }
            ToolResult::AbsolutePathError { path } => {
                format!("Path must be relative to project root: {}", path.display())
            }
            ToolResult::ReadFiles {
                loaded_files,
                failed_files,
            } => {
                let mut msg = String::new();
                if !loaded_files.is_empty() {
                    msg.push_str(&format!(
                        "Successfully loaded files: {}",
                        loaded_files
                            .keys()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                if !failed_files.is_empty() {
                    if !msg.is_empty() {
                        msg.push_str("\n");
                    }
                    msg.push_str("Failed to load: ");
                    msg.push_str(
                        &failed_files
                            .iter()
                            .map(|(p, e)| format!("{}: {}", p.display(), e))
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                }
                msg
            }
            ToolResult::ListFiles {
                expanded_paths,
                failed_paths,
                ..
            } => {
                let mut msg = String::new();
                if !expanded_paths.is_empty() {
                    msg.push_str(&format!("Successfully listed contents of: "));
                    msg.push_str(
                        &expanded_paths
                            .iter()
                            .map(|(path, _)| format!("{}", path.display()))
                            .collect::<Vec<_>>()
                            .join("; "),
                    );
                }
                if !failed_paths.is_empty() {
                    if !msg.is_empty() {
                        msg.push_str("\n");
                    }
                    msg.push_str("Failed listing: ");
                    msg.push_str(
                        &failed_paths
                            .iter()
                            .map(|(path, err)| format!("{}: {}", path, err))
                            .collect::<Vec<_>>()
                            .join("; "),
                    );
                }
                msg
            }
            ToolResult::SearchFiles { results, query } => {
                if results.is_empty() {
                    format!("No matches found for '{}'", query)
                } else {
                    let mut msg = format!("Found matches for '{}':\n", query);
                    for result in results {
                        msg.push_str(&format!(
                            "{}:{}-{}:\n",
                            result.file.display(),
                            result.start_line + 1,
                            result.start_line + result.line_content.len()
                        ));
                        for (i, line) in result.line_content.iter().enumerate() {
                            let line_prefix = if result.match_lines.contains(&i) {
                                ">"
                            } else {
                                " "
                            };
                            msg.push_str(&format!("{} {}\n", line_prefix, line));
                        }
                        msg.push('\n');
                    }
                    msg
                }
            }
            ToolResult::ExecuteCommand {
                stdout,
                stderr,
                error,
            } => {
                let mut msg = String::new();
                // Only add stdout without prefix if there are no errors
                if error.is_none() && stderr.is_empty() {
                    msg.push_str(stdout);
                } else {
                    // In case of errors, add more detailed output
                    if !stdout.is_empty() {
                        msg.push_str("Output:\n");
                        msg.push_str(stdout);
                    }
                    if !stderr.is_empty() {
                        if !msg.is_empty() {
                            msg.push_str("\n");
                        }
                        msg.push_str("Errors:\n");
                        msg.push_str(stderr);
                    }
                    if let Some(err) = error {
                        if !msg.is_empty() {
                            msg.push_str("\n");
                        }
                        msg.push_str(&format!("Command failed: {}", err));
                    }
                }
                msg
            }
            ToolResult::WriteFile { path, error, .. } => {
                if error.is_some() {
                    format!(
                        "Failed to write file {}: {}",
                        path.display(),
                        error.as_ref().unwrap()
                    )
                } else {
                    format!("Successfully wrote file: {}", path.display())
                }
            }
            ToolResult::ReplaceInFile { path, error, .. } => {
                if error.is_some() {
                    format!(
                        "Failed to replace in file {}: {}",
                        path.display(),
                        error.as_ref().unwrap()
                    )
                } else {
                    format!("Successfully replaced in file: {}", path.display())
                }
            }
            ToolResult::DeleteFiles { deleted, failed } => {
                let mut msg = String::new();
                if !deleted.is_empty() {
                    msg.push_str(&format!(
                        "Successfully deleted: {}",
                        deleted
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                if !failed.is_empty() {
                    if !msg.is_empty() {
                        msg.push_str("\n");
                    }
                    msg.push_str("Failed to delete: ");
                    msg.push_str(
                        &failed
                            .iter()
                            .map(|(p, e)| format!("{}: {}", p.display(), e))
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                }
                msg
            }
            ToolResult::Summarize { resources } => {
                format!("Created summaries for {} resources", resources.len())
            }
            ToolResult::AskUser { response } => response.clone(),
            ToolResult::MessageUser { result } => result.clone(),
            ToolResult::CompleteTask { result } => result.clone(),
            ToolResult::WebSearch { results, error, .. } => {
                if let Some(e) = error {
                    format!("Search failed: {}", e)
                } else if results.is_empty() {
                    "No search results found.".to_string()
                } else {
                    let mut msg = String::from("Search results:\n");
                    for result in results {
                        msg.push_str(&format!(
                            "- Title: {}\n  URL: {}\n  Snippet: {}\n\n",
                            result.title, result.url, result.snippet
                        ));
                    }
                    msg
                }
            }
            ToolResult::WebFetch { page, error } => {
                if let Some(e) = error {
                    format!("Failed to fetch page: {}", e)
                } else {
                    format!("Content from {}:\n{}", page.url, page.content)
                }
            }
        }
    }

    pub fn is_success(&self) -> bool {
        match self {
            ToolResult::OpenProject { error, .. } => error.is_none(),
            ToolResult::AbsolutePathError { .. } => false,
            ToolResult::ReadFiles {
                loaded_files,
                failed_files,
            } => !loaded_files.is_empty() && failed_files.is_empty(),
            ToolResult::ListFiles {
                expanded_paths,
                failed_paths,
                ..
            } => !expanded_paths.is_empty() && failed_paths.is_empty(),
            ToolResult::ExecuteCommand { error, .. } => error.is_none(),
            ToolResult::WriteFile { error, .. } => error.is_none(),
            ToolResult::ReplaceInFile { error, .. } => error.is_none(),
            ToolResult::DeleteFiles {
                deleted, failed, ..
            } => !deleted.is_empty() && failed.is_empty(),
            ToolResult::Summarize { .. } => true,
            _ => true,
        }
    }
}
