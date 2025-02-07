use crate::types::ToolResult;

impl ToolResult {
    // Format a user-facing message describing the result
    pub fn format_message(&self) -> String {
        match self {
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
                success,
                stdout,
                stderr,
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
                if !success {
                    if !msg.is_empty() {
                        msg.push_str("\n");
                    }
                    msg.push_str("Command failed");
                }
                msg
            }
            ToolResult::WriteFile { path, success, .. } => {
                if *success {
                    format!("Successfully wrote file: {}", path.display())
                } else {
                    format!("Failed to write file: {}", path.display())
                }
            }
            ToolResult::ReplaceInFile { path, success, .. } => {
                if *success {
                    format!("Successfully replaced in file: {}", path.display())
                } else {
                    format!("Failed to replaced in file: {}", path.display())
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
            ToolResult::ReadFiles { loaded_files, .. } => !loaded_files.is_empty(),
            ToolResult::ListFiles { .. } => true,
            ToolResult::SearchFiles { .. } => true,
            ToolResult::ExecuteCommand { success, .. } => *success,
            ToolResult::WriteFile { success, .. } => *success,
            ToolResult::ReplaceInFile { success, .. } => *success,
            ToolResult::DeleteFiles { deleted, .. } => !deleted.is_empty(),
            ToolResult::Summarize { .. } => true,
            ToolResult::AskUser { .. } => true,
            ToolResult::MessageUser { .. } => true,
            ToolResult::CompleteTask { .. } => true,
        }
    }
}
