use crate::tools::core::{Render, ResourcesTracker, Tool, ToolContext, ToolMode, ToolResult, ToolSpec};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::path::PathBuf;

// Input type for the delete_files tool
#[derive(Deserialize)]
pub struct DeleteFilesInput {
    pub project: String,
    pub paths: Vec<String>,
}

// Output type
pub struct DeleteFilesOutput {
    #[allow(dead_code)]
    pub project: String,
    pub deleted: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
}

// Render implementation for output formatting
impl Render for DeleteFilesOutput {
    fn status(&self) -> String {
        if self.failed.is_empty() {
            format!("Successfully deleted {} file(s)", self.deleted.len())
        } else {
            format!(
                "Deleted {} file(s), failed to delete {} file(s)",
                self.deleted.len(),
                self.failed.len()
            )
        }
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        let mut formatted = String::new();

        // Handle failed files first
        for (path, error) in &self.failed {
            formatted.push_str(&format!(
                "Failed to delete '{}': {}\n",
                path.display(),
                error
            ));
        }

        // List successfully deleted files
        if !self.deleted.is_empty() {
            formatted.push_str("Successfully deleted the following file(s):\n");
            for path in &self.deleted {
                formatted.push_str(&format!("- {}\n", path.display()));
            }
        }

        formatted
    }
}

// ToolResult implementation
impl ToolResult for DeleteFilesOutput {
    fn is_success(&self) -> bool {
        !self.deleted.is_empty() && self.failed.is_empty()
    }
}

// Tool implementation
pub struct DeleteFilesTool;

#[async_trait::async_trait]
impl Tool for DeleteFilesTool {
    type Input = DeleteFilesInput;
    type Output = DeleteFilesOutput;

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "delete_files",
            description: "Delete files from a specified project. This operation cannot be undone!",
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Name of the project containing the files"
                    },
                    "paths": {
                        "type": "array",
                        "description": "Paths to the files relative to the project root directory",
                        "items": {
                            "type": "string"
                        }
                    }
                },
                "required": ["project", "paths"]
            }),
            annotations: Some(serde_json::json!({
                "readOnlyHint": false,
                "destructiveHint": true,
                "idempotentHint": true
            })),
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
        let explorer = context
            .project_manager
            .get_explorer_for_project(&input.project)
            .map_err(|e| {
                anyhow!(
                    "Failed to get explorer for project {}: {}",
                    input.project,
                    e
                )
            })?;

        let mut deleted = Vec::new();
        let mut failed = Vec::new();

        // Process each path
        for path_str in input.paths {
            let path = PathBuf::from(&path_str);

            // Check for absolute paths
            if path.is_absolute() {
                failed.push((path.clone(), "Absolute paths are not allowed".to_string()));
                continue;
            }

            // Join with root_dir to get full path
            let full_path = explorer.root_dir().join(&path);

            // Try to delete the file
            match explorer.delete_file(&full_path) {
                Ok(_) => {
                    deleted.push(path.clone());

                    // If we have a working memory reference, remove the deleted file
                    if let Some(working_memory) = &mut context.working_memory {
                        // Remove from loaded resources
                        working_memory
                            .loaded_resources
                            .remove(&(input.project.clone(), path.clone()));

                        // Remove from summaries if it exists there
                        working_memory
                            .summaries
                            .remove(&(input.project.clone(), path.clone()));
                    }
                }
                Err(e) => {
                    failed.push((path, e.to_string()));
                }
            }
        }

        Ok(DeleteFilesOutput {
            project: input.project,
            deleted,
            failed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::tests::mocks::{MockExplorer, MockProjectManager};
    use crate::types::WorkingMemory;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_delete_files_output_rendering() {
        // Create output with some test data
        let deleted = vec![PathBuf::from("file1.txt"), PathBuf::from("file2.txt")];
        let failed = vec![(PathBuf::from("missing.txt"), "File not found".to_string())];

        let output = DeleteFilesOutput {
            project: "test-project".to_string(),
            deleted,
            failed,
        };

        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        // Verify rendering
        assert!(rendered.contains("Failed to delete 'missing.txt'"));
        assert!(rendered.contains("File not found"));
        assert!(rendered.contains("Successfully deleted the following file(s):"));
        assert!(rendered.contains("- file1.txt"));
        assert!(rendered.contains("- file2.txt"));
    }

    #[tokio::test]
    async fn test_delete_files_working_memory_update() -> Result<()> {
        // Create test files
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("./root/file1.txt"),
            "File 1 content".to_string(),
        );
        files.insert(
            PathBuf::from("./root/file2.txt"),
            "File 2 content".to_string(),
        );

        // Create a mock explorer with these files
        let explorer = MockExplorer::new(files, None);

        // Create a mock project manager
        let project_manager = Box::new(MockProjectManager::default().with_project(
            "test-project",
            PathBuf::from("./root"),
            explorer,
        ));

        // Create a command executor
        let command_executor = Box::new(crate::utils::DefaultCommandExecutor);

        // Create working memory with loaded resources
        let mut working_memory = WorkingMemory::default();

        // Add files to working memory
        working_memory.loaded_resources.insert(
            ("test-project".to_string(), PathBuf::from("file1.txt")),
            crate::types::LoadedResource::File("File 1 content".to_string()),
        );
        working_memory.loaded_resources.insert(
            ("test-project".to_string(), PathBuf::from("file2.txt")),
            crate::types::LoadedResource::File("File 2 content".to_string()),
        );

        // Also add a summary for one file
        working_memory.summaries.insert(
            ("test-project".to_string(), PathBuf::from("file1.txt")),
            "A summary of file 1".to_string(),
        );

        // Create a tool context with working memory
        let mut context = ToolContext {
            project_manager,
            command_executor,
            working_memory: Some(&mut working_memory),
        };

        // Create input
        let input = DeleteFilesInput {
            project: "test-project".to_string(),
            paths: vec!["file1.txt".to_string()],
        };

        // Execute tool
        let tool = DeleteFilesTool;
        let result = tool.execute(&mut context, input).await?;

        // Verify result
        assert_eq!(result.deleted.len(), 1);
        assert_eq!(result.deleted[0], PathBuf::from("file1.txt"));
        assert!(result.failed.is_empty());

        // Verify working memory updates
        assert_eq!(working_memory.loaded_resources.len(), 1);
        assert!(!working_memory
            .loaded_resources
            .contains_key(&("test-project".to_string(), PathBuf::from("file1.txt"))));
        assert!(working_memory
            .loaded_resources
            .contains_key(&("test-project".to_string(), PathBuf::from("file2.txt"))));

        // Verify summary was also removed
        assert!(working_memory.summaries.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn test_delete_files_error_handling() -> Result<()> {
        // Create test files (only one file)
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("./root/existing.txt"),
            "File content".to_string(),
        );

        // Create a mock explorer
        let explorer = MockExplorer::new(files, None);

        // Create a mock project manager
        let project_manager = Box::new(MockProjectManager::default().with_project(
            "test-project",
            PathBuf::from("./root"),
            explorer,
        ));

        // Create a command executor
        let command_executor = Box::new(crate::utils::DefaultCommandExecutor);

        // Create a tool context
        let mut context = ToolContext {
            project_manager,
            command_executor,
            working_memory: None,
        };

        // Create input with both existing and non-existing files
        let input = DeleteFilesInput {
            project: "test-project".to_string(),
            paths: vec![
                "existing.txt".to_string(),
                "non-existing.txt".to_string(),
                "/absolute/path.txt".to_string(),
            ],
        };

        // Execute tool
        let tool = DeleteFilesTool;
        let result = tool.execute(&mut context, input).await?;

        // Verify result
        assert_eq!(result.deleted.len(), 1);
        assert_eq!(result.deleted[0], PathBuf::from("existing.txt"));

        assert_eq!(result.failed.len(), 2);

        // One failure should be the non-existing file
        let non_existing = result
            .failed
            .iter()
            .find(|(path, _)| path == &PathBuf::from("non-existing.txt"));
        assert!(non_existing.is_some());

        // One failure should be the absolute path
        let absolute_path = result.failed.iter().find(|(path, error)| {
            path == &PathBuf::from("/absolute/path.txt")
                && error.contains("Absolute paths are not allowed")
        });
        assert!(absolute_path.is_some());

        Ok(())
    }
}
