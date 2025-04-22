use crate::tools::core::{Render, ResourcesTracker, Tool, ToolContext, ToolMode, ToolSpec};
use crate::types::FileTreeEntry;
use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;

// Input type for the list_files tool
#[derive(Deserialize)]
pub struct ListFilesInput {
    pub project: String,
    pub paths: Vec<String>,
    #[serde(default)]
    pub max_depth: Option<u64>,
}

// Output type
pub struct ListFilesOutput {
    pub expanded_paths: Vec<(PathBuf, FileTreeEntry)>,
    pub failed_paths: Vec<(String, String)>,
}

// Helper function to update file tree in working memory
fn update_tree_entry(
    parent: &mut FileTreeEntry,
    path: PathBuf,
    entry: FileTreeEntry,
) -> Result<()> {
    let components: Vec<_> = path.components().collect();
    if components.is_empty() {
        // Replace current node with new entry
        *parent = entry;
        return Ok(());
    }

    // Process path components
    let first = components[0].as_os_str().to_string_lossy().to_string();
    let remaining = if components.len() > 1 {
        let mut new_path = PathBuf::new();
        for component in &components[1..] {
            new_path.push(component);
        }
        Some(new_path)
    } else {
        None
    };

    // Insert or update the child node
    if let Some(remaining_path) = remaining {
        let child = parent
            .children
            .entry(first.clone())
            .or_insert_with(|| FileTreeEntry {
                name: first.clone(),
                entry_type: crate::types::FileSystemEntryType::Directory,
                children: std::collections::HashMap::new(),
                is_expanded: true,
            });
        update_tree_entry(child, remaining_path, entry)?;
    } else {
        parent.children.insert(first, entry);
    }

    Ok(())
}

// Render implementation for output formatting
impl Render for ListFilesOutput {
    fn status(&self) -> String {
        if self.failed_paths.is_empty() {
            format!("Listed files in {} path(s)", self.expanded_paths.len())
        } else {
            format!(
                "Listed files in {} path(s), {} path(s) failed",
                self.expanded_paths.len(),
                self.failed_paths.len()
            )
        }
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        let mut output = String::new();

        // Handle failed paths first
        if !self.failed_paths.is_empty() {
            output.push_str("Failed paths:\n");
            for (path, error) in &self.failed_paths {
                output.push_str(&format!("- '{}': {}\n", path, error));
            }
            output.push_str("\n");
        }

        // Format expanded paths
        if !self.expanded_paths.is_empty() {
            for (path, tree) in &self.expanded_paths {
                output.push_str(&format!("Path: {}\n", path.display()));

                // Use the built-in to_string method of FileTreeEntry
                output.push_str(&tree.to_string());
                output.push_str("\n");
            }
        } else if self.failed_paths.is_empty() {
            output.push_str("No files found.\n");
        }

        output
    }
}

// Tool implementation
pub struct ListFilesTool;

#[async_trait::async_trait]
impl Tool for ListFilesTool {
    type Input = ListFilesInput;
    type Output = ListFilesOutput;

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_files",
            description: "List files in directories within a specified project",
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Name of the project context"
                    },
                    "paths": {
                        "type": "array",
                        "description": "Directory paths relative to project root",
                        "items": {
                            "type": "string"
                        }
                    },
                    "max_depth": {
                        "type": "integer",
                        "description": "Optional: Maximum directory depth"
                    }
                },
                "required": ["project", "paths"]
            }),
            annotations: None,
            supported_modes: &[
                ToolMode::McpServer,
                ToolMode::WorkingMemoryAgent,
                ToolMode::MessageHistoryAgent,
            ],
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: Self::Input,
    ) -> Result<Self::Output> {
        // Get explorer for the specified project
        let mut explorer = match context
            .project_manager
            .get_explorer_for_project(&input.project)
        {
            Ok(explorer) => explorer,
            Err(e) => {
                return Ok(ListFilesOutput {
                    expanded_paths: Vec::new(),
                    failed_paths: vec![(
                        ".".to_string(),
                        format!(
                            "Failed to get explorer for project {}: {}",
                            input.project, e
                        ),
                    )],
                });
            }
        };

        let mut expanded_paths = Vec::new();
        let mut failed_paths = Vec::new();

        // Convert max_depth from u64 to usize if present
        let max_depth = input.max_depth.map(|d| d as usize);

        for path_str in input.paths {
            let path = PathBuf::from(&path_str);

            // Check if path is absolute and handle it properly
            if path.is_absolute() {
                failed_paths.push((
                    path.display().to_string(),
                    "Path must be relative to project root".to_string(),
                ));
                continue;
            }

            let full_path = explorer.root_dir().join(&path);
            match explorer.list_files(&full_path, max_depth) {
                Ok(tree_entry) => {
                    expanded_paths.push((path, tree_entry));
                }
                Err(e) => {
                    failed_paths.push((path_str, e.to_string()));
                }
            }
        }

        // If we have a working memory reference, update it with the expanded paths
        if let Some(working_memory) = &mut context.working_memory {
            // Create file tree for this project if it doesn't exist yet
            let file_tree = working_memory
                .file_trees
                .entry(input.project.clone())
                .or_insert_with(|| FileTreeEntry {
                    name: input.project.clone(),
                    entry_type: crate::types::FileSystemEntryType::Directory,
                    children: std::collections::HashMap::new(),
                    is_expanded: true,
                });

            // Update file tree with each entry
            for (path, entry) in &expanded_paths {
                if let Err(e) = update_tree_entry(file_tree, path.clone(), entry.clone()) {
                    eprintln!("Error updating tree entry: {}", e);
                    // Continue with other entries even if one fails
                }
            }

            // Store expanded directories for this project
            let project_paths = working_memory
                .expanded_directories
                .entry(input.project.clone())
                .or_insert_with(Vec::new);

            // Add all paths that were listed for this project
            for (path, _) in &expanded_paths {
                if !project_paths.contains(path) {
                    project_paths.push(path.clone());
                }
            }

            // Make sure project is in available_projects list
            if !working_memory.available_projects.contains(&input.project) {
                working_memory
                    .available_projects
                    .push(input.project.clone());
            }
        }

        Ok(ListFilesOutput {
            expanded_paths,
            failed_paths,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FileSystemEntryType;
    use std::collections::HashMap;

    // Helper to create a simple file tree for testing
    fn create_test_file_tree() -> FileTreeEntry {
        let mut src_children = HashMap::new();
        src_children.insert(
            "main.rs".to_string(),
            FileTreeEntry {
                name: "main.rs".to_string(),
                entry_type: FileSystemEntryType::File,
                children: HashMap::new(),
                is_expanded: false,
            },
        );

        let mut root_children = HashMap::new();
        root_children.insert(
            "src".to_string(),
            FileTreeEntry {
                name: "src".to_string(),
                entry_type: FileSystemEntryType::Directory,
                children: src_children,
                is_expanded: true,
            },
        );
        root_children.insert(
            "README.md".to_string(),
            FileTreeEntry {
                name: "README.md".to_string(),
                entry_type: FileSystemEntryType::File,
                children: HashMap::new(),
                is_expanded: false,
            },
        );

        FileTreeEntry {
            name: "test-root".to_string(),
            entry_type: FileSystemEntryType::Directory,
            children: root_children,
            is_expanded: true,
        }
    }

    #[tokio::test]
    async fn test_list_files_output_rendering() {
        // Create output with test data
        let test_tree = create_test_file_tree();

        let expanded_paths = vec![(PathBuf::from("test-dir"), test_tree)];

        let failed_paths = vec![("nonexistent".to_string(), "Directory not found".to_string())];

        let output = ListFilesOutput {
            expanded_paths,
            failed_paths,
        };

        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        // Verify rendering
        assert!(rendered.contains("Failed paths:"));
        assert!(rendered.contains("- 'nonexistent': Directory not found"));
        assert!(rendered.contains("Path: test-dir"));
        assert!(rendered.contains("src"));
        assert!(rendered.contains("main.rs"));
        assert!(rendered.contains("README.md"));
    }
}
