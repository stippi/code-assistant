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
            ToolResult::OpenProject {
                name,
                error,
                ..
            } => {
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
                    let mut msg = format!("Found {} matches for '{}':\n", results.len(), query);
                    for result in results {
                        msg.push_str(&format!(
                            "{}:{}: {}\n",
                            result.file.display(),
                            result.line_number,
                            result.line_content
                        ));
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
                if error.is_some() {
                    if !msg.is_empty() {
                        msg.push_str("\n");
                    }
                    msg.push_str(&format!("Command failed: {}", error.as_ref().unwrap()));
                }
                msg
            }
            ToolResult::WriteFile { path, error, .. } => {
                if error.is_some() {
                    format!("Failed to write file {}: {}", path.display(), error.as_ref().unwrap())
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
            ToolResult::Summarize { files } => {
                format!("Created summaries for {} files", files.len())
            }
            ToolResult::AskUser { response } => response.clone(),
            ToolResult::MessageUser { result } => result.clone(),
            ToolResult::CompleteTask { result } => result.clone(),
        }
    }

    pub fn is_success(&self) -> bool {
        match self {
            ToolResult::ListProjects { .. } => true,
            ToolResult::OpenProject { error, .. } => error.is_none(),
            ToolResult::ReadFiles { loaded_files, .. } => !loaded_files.is_empty(),
            ToolResult::ListFiles { .. } => true,
            ToolResult::SearchFiles { .. } => true,
            ToolResult::ExecuteCommand { error, .. } => error.is_none(),
            ToolResult::WriteFile { error, .. } => error.is_none(),
            ToolResult::ReplaceInFile { error, .. } => error.is_none(),
            ToolResult::DeleteFiles { deleted, .. } => !deleted.is_empty(),
            ToolResult::Summarize { .. } => true,
            ToolResult::AskUser { .. } => true,
            ToolResult::MessageUser { .. } => true,
            ToolResult::CompleteTask { .. } => true,
        }
    }
}
