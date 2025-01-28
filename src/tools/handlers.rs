use crate::tools::ToolResultHandler;
use crate::types::{ActionResult, FileTreeEntry, ToolResult, WorkingMemory};
use crate::utils::format_with_line_numbers;
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
    async fn handle_result(&mut self, result: &ActionResult) -> Result<String> {
        // Update working memory if tool was successful
        if result.result.is_success() {
            match &result.result {
                ToolResult::ListFiles { expanded_paths, .. } => {
                    // Update working memory file tree with each entry
                    if let Some(file_tree) = &mut self.working_memory.file_tree {
                        for (path, entry) in expanded_paths {
                            update_tree_entry(file_tree, path, entry.clone())?;
                        }
                    }
                }
                ToolResult::ReadFiles { loaded_files, .. } => {
                    self.working_memory
                        .loaded_files
                        .extend(loaded_files.clone());
                }
                ToolResult::Summarize { files } => {
                    for (path, summary) in files {
                        self.working_memory.loaded_files.remove(path);
                        self.working_memory
                            .file_summaries
                            .insert(path.clone(), summary.clone());
                    }
                }
                ToolResult::UpdateFile { path, content, .. } => {
                    // Update working memory if file was loaded
                    if self.working_memory.loaded_files.contains_key(path) {
                        self.working_memory
                            .loaded_files
                            .insert(path.clone(), content.clone());
                    }
                }
                ToolResult::WriteFile { path, .. } => {
                    // Remove any existing content/summary since file is new/overwritten
                    self.working_memory.loaded_files.remove(path);
                    self.working_memory.file_summaries.remove(path);
                }
                _ => {}
            }
        }

        Ok(result.result.format_message())
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
    async fn handle_result(&mut self, result: &ActionResult) -> Result<String> {
        match &result.result {
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
            ToolResult::ReadFiles { loaded_files, .. } => {
                // Format detailed output with file contents
                let mut output = String::new();
                for (path, content) in loaded_files {
                    output.push_str(&format!(
                        "File: {}\n{}\n",
                        path.display(),
                        format_with_line_numbers(content)
                    ));
                }
                Ok(output)
            }
            // All other tools use standard message
            _ => Ok(result.result.format_message()),
        }
    }
}

pub struct ReplayToolHandler {
    working_memory: WorkingMemory,
}

impl ReplayToolHandler {
    pub fn new(working_memory: WorkingMemory) -> Self {
        Self { working_memory }
    }

    pub fn into_memory(self) -> WorkingMemory {
        self.working_memory
    }
}

#[async_trait]
impl ToolResultHandler for ReplayToolHandler {
    async fn handle_result(&mut self, result: &ActionResult) -> Result<String> {
        // Only update working memory, ignore filesystem effects
        if result.result.is_success() {
            match &result.result {
                ToolResult::ReadFiles { loaded_files, .. } => {
                    self.working_memory
                        .loaded_files
                        .extend(loaded_files.clone());
                }
                ToolResult::Summarize { files } => {
                    for (path, summary) in files {
                        self.working_memory.loaded_files.remove(path);
                        self.working_memory
                            .file_summaries
                            .insert(path.clone(), summary.clone());
                    }
                }
                ToolResult::UpdateFile {
                    path,
                    content,
                    success: true,
                    ..
                } => {
                    if self.working_memory.loaded_files.contains_key(path) {
                        self.working_memory
                            .loaded_files
                            .insert(path.clone(), content.clone());
                    }
                }
                ToolResult::WriteFile { path, .. } => {
                    // Just remove from working memory if files were loaded
                    self.working_memory.loaded_files.remove(path);
                    self.working_memory.file_summaries.remove(path);
                }
                ToolResult::ListFiles { expanded_paths, .. } => {
                    // Update file tree with the entries
                    if let Some(file_tree) = &mut self.working_memory.file_tree {
                        for (path, entry) in expanded_paths {
                            update_tree_entry(file_tree, path, entry.clone())?;
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(result.result.format_message())
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
