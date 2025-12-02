use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolScope, ToolSpec,
};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;

// Input type for the delete_files tool
#[derive(Deserialize, Serialize)]
pub struct DeleteFilesInput {
    pub project: String,
    pub paths: Vec<String>,
}

// Output type
#[derive(Serialize, Deserialize)]
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
            parameters_schema: json!({
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
            annotations: Some(json!({
                "readOnlyHint": false,
                "destructiveHint": true,
                "idempotentHint": true
            })),
            supported_scopes: &[
                ToolScope::McpServer,
                ToolScope::Agent,
                ToolScope::AgentWithDiffBlocks,
            ],
            hidden: false,
            title_template: Some("Deleting {paths}"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
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
        for path_str in input.paths.clone() {
            let path = PathBuf::from(&path_str);

            // Check for absolute paths
            if path.is_absolute() {
                failed.push((path.clone(), "Absolute paths are not allowed".to_string()));
                continue;
            }

            // Join with root_dir to get full path
            let full_path = explorer.root_dir().join(&path);

            // Try to delete the file
            match explorer.delete_file(&full_path).await {
                Ok(_) => {
                    deleted.push(path.clone());

                    // Emit resource event
                    if let Some(ui) = context.ui {
                        let _ = ui
                            .send_event(crate::ui::UiEvent::ResourceDeleted {
                                project: input.project.clone(),
                                path: path.clone(),
                            })
                            .await;
                    }
                }
                Err(e) => {
                    failed.push((path, e.to_string()));
                }
            }
        }

        Ok(DeleteFilesOutput {
            project: input.project.clone(),
            deleted,
            failed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mocks::ToolTestFixture;

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
    async fn test_delete_files_emits_resource_deleted_event() -> Result<()> {
        // Create test fixture with UI
        let mut fixture = ToolTestFixture::with_files(vec![
            ("file1.txt".to_string(), "File 1 content".to_string()),
            ("file2.txt".to_string(), "File 2 content".to_string()),
        ])
        .with_ui();

        let mut context = fixture.context();

        // Create input
        let mut input = DeleteFilesInput {
            project: "test-project".to_string(),
            paths: vec!["file1.txt".to_string()],
        };

        // Execute tool
        let tool = DeleteFilesTool;
        let result = tool.execute(&mut context, &mut input).await?;

        // Verify result
        assert_eq!(result.deleted.len(), 1);
        assert_eq!(result.deleted[0], PathBuf::from("file1.txt"));
        assert!(result.failed.is_empty());

        // Drop context to release borrow
        drop(context);

        // Verify ResourceDeleted event was emitted
        let events = fixture.ui().unwrap().events();
        assert!(events.iter().any(|e| matches!(
            e,
            crate::ui::UiEvent::ResourceDeleted { project, path }
            if project == "test-project" && path == &PathBuf::from("file1.txt")
        )));

        Ok(())
    }

    #[tokio::test]
    async fn test_delete_files_error_handling() -> Result<()> {
        // Create test fixture with one file
        let mut fixture = ToolTestFixture::with_files(vec![(
            "existing.txt".to_string(),
            "File content".to_string(),
        )]);
        let mut context = fixture.context();

        // Create input with both existing and non-existing files
        let mut input = DeleteFilesInput {
            project: "test-project".to_string(),
            paths: vec![
                "existing.txt".to_string(),
                "non-existing.txt".to_string(),
                "/absolute/path.txt".to_string(),
            ],
        };

        // Execute tool
        let tool = DeleteFilesTool;
        let result = tool.execute(&mut context, &mut input).await?;

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
