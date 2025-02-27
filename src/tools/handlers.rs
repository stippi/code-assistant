use crate::explorer::Explorer;
use crate::tools::ToolResultHandler;
use crate::types::{CodeExplorer, FileTreeEntry, LoadedResource, ToolResult, WorkingMemory};
use crate::PathBuf;
use anyhow::Result;
use async_trait::async_trait;

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
        if result.is_success() {
            match &result {
                ToolResult::ListFiles { expanded_paths, .. } => {
                    // Update working memory file tree with each entry
                    if let Some(file_tree) = &mut self.working_memory.file_tree {
                        for (path, entry) in expanded_paths {
                            update_tree_entry(file_tree, path, entry.clone())?;
                        }
                    }
                }
                ToolResult::ReadFiles { loaded_files, .. } => {
                    for (path, content) in loaded_files {
                        self.working_memory
                            .add_resource(path.clone(), LoadedResource::File(content.clone()));
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
                    self.working_memory.loaded_resources.insert(
                        path,
                        LoadedResource::WebSearch {
                            query: query.clone(),
                            results: results.clone(),
                        },
                    );
                }
                ToolResult::WebFetch { page, error: None } => {
                    // Use the URL as path (normalized)
                    let path = PathBuf::from(page.url.replace([':', '/', '?', '#'], "_"));
                    self.working_memory
                        .loaded_resources
                        .insert(path, LoadedResource::WebPage(page.clone()));
                }
                ToolResult::Summarize { resources } => {
                    for (path, summary) in resources {
                        self.working_memory.loaded_resources.remove(path);
                        self.working_memory
                            .summaries
                            .insert(path.clone(), summary.clone());
                    }
                }
                ToolResult::ReplaceInFile { path, content, .. } => {
                    // Update working memory if file was loaded
                    self.working_memory
                        .update_resource(path, LoadedResource::File(content.clone()));
                }
                ToolResult::WriteFile {
                    path,
                    content,
                    error: None,
                    ..
                } => {
                    // Remove any existing summary since file is new/overwritten
                    self.working_memory.summaries.remove(path);
                    // Make this file part of the loaded files
                    self.working_memory
                        .add_resource(path.clone(), LoadedResource::File(content.clone()));
                }
                ToolResult::DeleteFiles { deleted, .. } => {
                    for path in deleted {
                        self.working_memory.loaded_resources.remove(path);
                        self.working_memory.summaries.remove(path);
                    }
                }
                _ => {}
            }
        }

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
        match result {
            ToolResult::OpenProject { path, name, .. } => {
                if let Some(project_path) = path {
                    // Create a temporary Explorer to list the root folder
                    let mut explorer = Explorer::new(project_path.clone());
                    match explorer.create_initial_tree(1) {
                        Ok(tree) => {
                            // Use the listing as part of the result
                            let mut output = format!("Successfully opened project '{}'\n\n", name);
                            output.push_str(&tree.to_string());
                            Ok(output)
                        }
                        Err(_) => {
                            // Return the standard message when listing fails
                            Ok(result.format_message())
                        }
                    }
                } else {
                    Ok(result.format_message())
                }
            }
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
                loaded_files,
                failed_files,
            } => {
                // Format detailed output with file contents
                let mut output = String::new();
                if !failed_files.is_empty() {
                    for (path, error) in failed_files {
                        output.push_str(&format!(
                            "Failed to load '{}': {}\n",
                            path.display(),
                            error
                        ));
                    }
                }
                if !loaded_files.is_empty() {
                    output.push_str(&format!("Successfully loaded the following file(s):\n"));
                    for (path, content) in loaded_files {
                        output.push_str(&format!(
                            "-----[ {} ]-----\n{}\n",
                            path.display(),
                            content
                        ));
                    }
                }
                Ok(output)
            }
            ToolResult::WebFetch { page, error } => {
                // Format detailed output with page contents
                let mut output = String::new();
                if let Some(e) = error {
                    output.push_str(&format!("Failed to fetch page: {}", e));
                } else {
                    output.push_str(&format!("Page fetched successfully:\n{}\n", page.content));
                }
                Ok(output)
            }
            // All other tools use standard message
            _ => Ok(result.format_message()),
        }
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
