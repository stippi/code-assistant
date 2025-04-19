use crate::tools::core::{Render, ResourcesTracker, Tool, ToolContext, ToolMode, ToolSpec};
use crate::types::LoadedResource;
use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;

// Input type for the write_file tool
#[derive(Deserialize)]
pub struct WriteFileInput {
    pub project: String,
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub append: bool,
}

// Output type
pub struct WriteFileOutput {
    pub path: PathBuf,
    pub content: String,
    pub error: Option<String>,
}

// Render implementation for output formatting
impl Render for WriteFileOutput {
    fn status(&self) -> String {
        if self.error.is_none() {
            format!("Successfully wrote to file: {}", self.path.display())
        } else {
            format!(
                "Failed to write to file {}: {}",
                self.path.display(),
                self.error.as_ref().unwrap()
            )
        }
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        if let Some(error) = &self.error {
            format!("Failed to write file '{}': {}", self.path.display(), error)
        } else {
            format!(
                "Successfully wrote {} bytes to file '{}'",
                self.content.len(),
                self.path.display()
            )
        }
    }
}

// Tool implementation
pub struct WriteFileTool;

#[async_trait::async_trait]
impl Tool for WriteFileTool {
    type Input = WriteFileInput;
    type Output = WriteFileOutput;

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file",
            description: include_str!("description.md"),
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Name of the project context"
                    },
                    "path": {
                        "type": "string",
                        "description": "Path to create or overwrite (relative to project root)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write (make sure it's the complete file)"
                    },
                    "append": {
                        "type": "boolean",
                        "description": "Optional: Whether to append to the file. Default is false.",
                        "default": false
                    }
                },
                "required": ["project", "path", "content"]
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
        let explorer = match context
            .project_manager
            .get_explorer_for_project(&input.project)
        {
            Ok(explorer) => explorer,
            Err(e) => {
                return Ok(WriteFileOutput {
                    path: PathBuf::from(&input.path),
                    content: String::new(), // Empty content on error
                    error: Some(format!(
                        "Failed to get explorer for project {}: {}",
                        input.project, e
                    )),
                });
            }
        };

        // Check for absolute path
        let path = PathBuf::from(&input.path);
        if path.is_absolute() {
            return Ok(WriteFileOutput {
                path,
                content: String::new(),
                error: Some("Absolute paths are not allowed".to_string()),
            });
        }

        // Join with root_dir to get full path
        let full_path = explorer.root_dir().join(&path);

        // Write the file
        match explorer.write_file(&full_path, &input.content, input.append) {
            Ok(full_content) => {
                // If we have a working memory reference, update it with the written file
                if let Some(working_memory) = &mut context.working_memory {
                    // Remove any existing summary since file is new/overwritten
                    working_memory
                        .summaries
                        .remove(&(input.project.clone(), path.clone()));

                    // Always update the working memory with the complete content
                    // For both new files and append operations
                    working_memory.loaded_resources.insert(
                        (input.project.clone(), path.clone()),
                        LoadedResource::File(full_content.clone()),
                    );
                }

                Ok(WriteFileOutput {
                    path,
                    content: input.content,
                    error: None,
                })
            }
            Err(e) => Ok(WriteFileOutput {
                path,
                content: String::new(), // Empty content on error
                error: Some(e.to_string()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::tests::mocks::{MockExplorer, MockProjectManager};
    use crate::types::WorkingMemory;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_write_file_output_rendering() {
        // Success case
        let output = WriteFileOutput {
            path: PathBuf::from("test.txt"),
            content: "Test content".to_string(),
            error: None,
        };

        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);
        assert!(rendered.contains("Successfully wrote"));
        assert!(rendered.contains("test.txt"));

        // Error case
        let output_error = WriteFileOutput {
            path: PathBuf::from("test.txt"),
            content: String::new(),
            error: Some("File not writable".to_string()),
        };

        let rendered_error = output_error.render(&mut tracker);
        assert!(rendered_error.contains("Failed to write file"));
        assert!(rendered_error.contains("File not writable"));
    }

    #[tokio::test]
    async fn test_write_file_updates_memory() -> Result<()> {
        // Create a tool registry (not needed for this test but kept for consistency)
        let write_file_tool = WriteFileTool;

        // Create test files
        let mut files = HashMap::new();

        // Create a mock explorer with these files
        let explorer = MockExplorer::new(files, None);

        // Create a mock project manager with our test files
        let project_manager = Box::new(
            MockProjectManager::default()
                .with_project("test-project", PathBuf::from("./root"), explorer)
        );

        // Create working memory
        let mut working_memory = WorkingMemory::default();

        // Create a tool context with working memory
        let mut context = ToolContext::<'_> {
            project_manager,
            working_memory: Some(&mut working_memory),
        };

        // Parameters for write_file
        let input = WriteFileInput {
            project: "test-project".to_string(),
            path: "test.txt".to_string(),
            content: "Test content".to_string(),
            append: false,
        };

        // Execute the tool
        let result = write_file_tool.execute(&mut context, input).await?;

        // Check the result
        assert!(result.error.is_none());

        // Verify that the file was added to working memory
        assert_eq!(working_memory.loaded_resources.len(), 1);

        // Check that the file is in the working memory
        let resource_key = ("test-project".to_string(), PathBuf::from("test.txt"));
        assert!(working_memory.loaded_resources.contains_key(&resource_key));

        // Check that the content matches
        if let Some(LoadedResource::File(content)) =
            working_memory.loaded_resources.get(&resource_key)
        {
            assert_eq!(content, "Test content");
        } else {
            panic!("Expected file resource in working memory");
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_write_file_append_has_memory_update() -> Result<()> {
        // Create a tool registry (not needed for this test but kept for consistency)
        let write_file_tool = WriteFileTool;

        // Create test files with existing content
        let mut files = HashMap::new();
        files.insert(PathBuf::from("./root/test.txt"), "Initial content".to_string());

        // Create a mock explorer with these files
        let explorer = MockExplorer::new(files, None);

        // Create a mock project manager with our test files
        let project_manager = Box::new(
            MockProjectManager::default()
                .with_project("test-project", PathBuf::from("./root"), explorer)
        );

        // Create working memory
        let mut working_memory = WorkingMemory::default();

        // Create a tool context with working memory
        let mut context = ToolContext::<'_> {
            project_manager,
            working_memory: Some(&mut working_memory),
        };

        // Parameters for write_file with append=true
        let input = WriteFileInput {
            project: "test-project".to_string(),
            path: "test.txt".to_string(),
            content: "Test content".to_string(),
            append: true, // Append mode
        };

        // Execute the tool
        let result = write_file_tool.execute(&mut context, input).await?;

        // Check the result
        assert!(result.error.is_none());

        // Verify that the file WAS added to working memory (we fixed the behavior)
        assert_eq!(working_memory.loaded_resources.len(), 1);

        // Check that the file is in the working memory
        let resource_key = ("test-project".to_string(), PathBuf::from("test.txt"));
        assert!(working_memory.loaded_resources.contains_key(&resource_key));

        // Check that the content is the combined content (initial + appended)
        if let Some(LoadedResource::File(content)) = working_memory.loaded_resources.get(&resource_key) {
            assert!(content.contains("Initial content"));
            assert!(content.contains("Test content"));
        } else {
            panic!("Expected file resource in working memory");
        }

        Ok(())
    }
}
