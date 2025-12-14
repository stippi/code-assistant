use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolScope, ToolSpec,
};
use anyhow::Result;
use fs_explorer::FileTreeEntry;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;

// Input type for the list_files tool
#[derive(Deserialize, Serialize)]
pub struct ListFilesInput {
    pub project: String,
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u64>,
}

// Output type
#[derive(Serialize, Deserialize)]
pub struct ListFilesOutput {
    pub expanded_paths: Vec<(PathBuf, FileTreeEntry)>,
    pub failed_paths: Vec<(String, String)>,
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
                output.push_str(&format!("- '{path}': {error}\n"));
            }
            output.push('\n');
        }

        // Format expanded paths
        if !self.expanded_paths.is_empty() {
            for (path, tree) in &self.expanded_paths {
                output.push_str(&format!("Path: {}\n", path.display()));

                // Use the built-in to_string method of FileTreeEntry
                output.push_str(&tree.to_string());
                output.push('\n');
            }
        } else if self.failed_paths.is_empty() {
            output.push_str("No files found.\n");
        }

        output
    }
}

// ToolResult implementation
impl ToolResult for ListFilesOutput {
    fn is_success(&self) -> bool {
        !self.expanded_paths.is_empty()
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
            parameters_schema: json!({
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
            annotations: Some(json!({
                "readOnlyHint": true
            })),
            supported_scopes: &[
                ToolScope::McpServer,
                ToolScope::Agent,
                ToolScope::AgentWithDiffBlocks,
            ],
            hidden: false,
            title_template: Some("Listing files in {paths}"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
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

        for path_str in input.paths.clone() {
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
            match explorer.list_files(&full_path, max_depth).await {
                Ok(tree_entry) => {
                    expanded_paths.push((path, tree_entry));
                }
                Err(e) => {
                    failed_paths.push((path_str, e.to_string()));
                }
            }
        }

        // Emit directory listed events for each successful path
        if let Some(ui) = context.ui {
            for (path, _) in &expanded_paths {
                let _ = ui
                    .send_event(crate::ui::UiEvent::DirectoryListed {
                        project: input.project.clone(),
                        path: path.clone(),
                    })
                    .await;
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
    use fs_explorer::FileSystemEntryType;
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
