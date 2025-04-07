use crate::tools::ToolResultHandler;
use crate::types::{FileTreeEntry, LoadedResource, ToolResult, WorkingMemory};
use crate::PathBuf;
use anyhow::Result;
use async_trait::async_trait;

// Helper functions to avoid duplicated code
fn format_output_for_result(result: &ToolResult) -> Result<String> {
    match result {
        ToolResult::ListFiles { expanded_paths, .. } => {
            let mut output = String::new();
            for (path, entry) in expanded_paths {
                output.push_str(&format!(
                    "Contents of {}:\n{}\n",
                    path.display(),
                    entry.to_string()
                ));
            }
            Ok(output)
        }
        ToolResult::ReadFiles {
            project,
            loaded_files,
            failed_files,
        } => {
            // Format detailed output with file contents
            let mut output = String::new();
            if !failed_files.is_empty() {
                for (path, error) in failed_files {
                    output.push_str(&format!(
                        "Failed to load '{}' in project '{}': {}\n",
                        path.display(),
                        project,
                        error
                    ));
                }
            }
            if !loaded_files.is_empty() {
                output.push_str(&format!("Successfully loaded the following file(s):\n"));
                for (path, content) in loaded_files {
                    //output.push_str(&format!("-----[ {} ]-----\n{}\n", path.display(), content));
                    output.push_str(&format!(
                        ">>>>> FILE: {}\n{}\n<<<<< END FILE\n",
                        path.display(),
                        content
                    ));
                }
            }
            Ok(output)
        }
        ToolResult::ReplaceInFile { error, .. } => {
            // Handle special case for Search Block Not Found error
            if let Some(error_value) = error {
                if let crate::utils::FileUpdaterError::SearchBlockNotFound(_, _) = error_value {
                    // Use the content from the ToolResult
                    let mut output = format!("Failed to replace in file: {}\n\n", error_value);
                    output.push_str(&format!(
                        "Please adjust your SEARCH block to the current contents of the file."
                    ));
                    return Ok(output);
                }
            }
            // Default to standard message for other errors
            Ok(result.format_message())
        }
        ToolResult::WebFetch { page, error } => {
            // Format detailed output with page contents
            let mut output = String::new();
            if let Some(e) = error {
                output.push_str(&format!("Failed to fetch page: {}", e));
            } else {
                output.push_str(&format!(
                    "Page fetched successfully:\n>>>>> CONTENT:\n{}\n<<<<< END CONTENT",
                    page.content
                ));
            }
            Ok(output)
        }
        // All other tools use standard message
        _ => Ok(result.format_message()),
    }
}

fn update_working_memory(working_memory: &mut WorkingMemory, result: &ToolResult) -> Result<()> {
    if result.is_success() {
        match result {
            ToolResult::UpdatePlan { plan } => {
                working_memory.plan = plan.clone();
            }

            ToolResult::ListFiles {
                project,
                expanded_paths,
                ..
            } => {
                // Store expanded directories for this project
                let project_paths = working_memory
                    .expanded_directories
                    .entry(project.clone())
                    .or_insert_with(Vec::new);

                // Add all paths that were listed for this project
                for (path, _) in expanded_paths {
                    if !project_paths.contains(path) {
                        project_paths.push(path.clone());
                    }
                }

                // Create file tree for this project if it doesn't exist yet
                let file_tree = working_memory
                    .file_trees
                    .entry(project.clone())
                    .or_insert_with(|| FileTreeEntry {
                        name: project.clone(),
                        entry_type: crate::types::FileSystemEntryType::Directory,
                        children: std::collections::HashMap::new(),
                        is_expanded: true,
                    });

                // Update file tree with each entry
                for (path, entry) in expanded_paths {
                    update_tree_entry(file_tree, path, entry.clone())?;
                }

                // Make sure project is in available_projects list
                if !working_memory.available_projects.contains(project) {
                    working_memory.available_projects.push(project.clone());
                }
            }
            ToolResult::ReadFiles {
                project,
                loaded_files,
                ..
            } => {
                for (path, content) in loaded_files {
                    working_memory.add_resource(
                        project.clone(),
                        path.clone(),
                        LoadedResource::File(content.clone()),
                    );
                }
            }
            ToolResult::WebSearch {
                results,
                query,
                error: None,
            } => {
                // Use a synthetic path that includes the query
                let path = PathBuf::from(format!(
                    "web-search-{}",
                    percent_encoding::utf8_percent_encode(
                        &query,
                        percent_encoding::NON_ALPHANUMERIC
                    )
                ));

                // Use "web" as the project name for web resources
                let project = "web".to_string();
                working_memory.loaded_resources.insert(
                    (project, path),
                    LoadedResource::WebSearch {
                        query: query.clone(),
                        results: results.clone(),
                    },
                );
            }
            ToolResult::WebFetch { page, error: None } => {
                // Use the URL as path (normalized)
                let path = PathBuf::from(page.url.replace([':', '/', '?', '#'], "_"));

                // Use "web" as the project name for web resources
                let project = "web".to_string();
                working_memory
                    .loaded_resources
                    .insert((project, path), LoadedResource::WebPage(page.clone()));
            }
            ToolResult::Summarize { resources } => {
                for ((project, path), summary) in resources {
                    // Remove from loaded resources
                    working_memory
                        .loaded_resources
                        .remove(&(project.clone(), path.clone()));

                    // Add to summaries
                    working_memory
                        .summaries
                        .insert((project.to_string(), path.clone()), summary.clone());
                }
            }
            ToolResult::ReplaceInFile {
                project,
                path,
                content,
                ..
            } => {
                // Update working memory if file was loaded
                working_memory.update_resource(
                    &project,
                    path,
                    LoadedResource::File(content.clone()),
                );
            }
            ToolResult::WriteFile {
                project,
                path,
                content,
                error: None,
                ..
            } => {
                // Remove any existing summary since file is new/overwritten
                working_memory
                    .summaries
                    .remove(&(project.clone(), path.clone()));

                // Make this file part of the loaded files
                working_memory.add_resource(
                    project.clone(),
                    path.clone(),
                    LoadedResource::File(content.clone()),
                );
            }
            ToolResult::DeleteFiles {
                project, deleted, ..
            } => {
                for path in deleted {
                    working_memory
                        .loaded_resources
                        .remove(&(project.clone(), path.clone()));
                    working_memory
                        .summaries
                        .remove(&(project.clone(), path.clone()));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub struct AgentToolHandler<'a> {
    working_memory: &'a mut WorkingMemory,
}

impl<'a> AgentToolHandler<'a> {
    pub fn new(working_memory: &'a mut WorkingMemory) -> Self {
        Self { working_memory }
    }
}

#[async_trait::async_trait]
impl<'a> ToolResultHandler for AgentToolHandler<'a> {
    async fn handle_result(&mut self, result: &ToolResult) -> Result<String> {
        // Update working memory if tool was successful
        update_working_memory(self.working_memory, result)?;
        Ok(result.format_message())
    }
}

pub struct MCPToolHandler;

impl MCPToolHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolResultHandler for MCPToolHandler {
    async fn handle_result(&mut self, result: &ToolResult) -> Result<String> {
        format_output_for_result(result)
    }
}

/// Handler for the Agent Chat mode that combines working memory update with user-friendly output
pub struct AgentChatToolHandler<'a> {
    working_memory: &'a mut WorkingMemory,
}

impl<'a> AgentChatToolHandler<'a> {
    pub fn new(working_memory: &'a mut WorkingMemory) -> Self {
        Self { working_memory }
    }
}

#[async_trait]
impl<'a> ToolResultHandler for AgentChatToolHandler<'a> {
    async fn handle_result(&mut self, result: &ToolResult) -> Result<String> {
        // First update the working memory
        update_working_memory(self.working_memory, result)?;

        // Then format the output like MCP handler
        format_output_for_result(result)
    }
}

fn update_tree_entry(
    tree: &mut FileTreeEntry,
    path: &PathBuf,
    new_entry: FileTreeEntry,
) -> Result<()> {
    let components: Vec<_> = path.components().collect();
    let mut current = tree;

    for (i, component) in components.iter().enumerate() {
        let name = component.as_os_str().to_string_lossy().to_string();
        let is_last = i == components.len() - 1;

        if is_last {
            current.children.insert(name, new_entry.clone());
            break;
        }

        current = current
            .children
            .get_mut(&name)
            .ok_or_else(|| anyhow::anyhow!("Path component not found: {}", name))?;
    }

    Ok(())
}
