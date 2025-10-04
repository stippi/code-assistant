use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolScope, ToolSpec,
};
use crate::types::LoadedResource;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;

// Input type for the write_file tool
#[derive(Deserialize, Serialize)]
pub struct WriteFileInput {
    pub project: String,
    pub path: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub append: bool,
}

// Output type
#[derive(Serialize, Deserialize)]
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

// ToolResult implementation
impl ToolResult for WriteFileOutput {
    fn is_success(&self) -> bool {
        self.error.is_none()
    }
}

// Tool implementation
pub struct WriteFileTool;

#[async_trait::async_trait]
impl Tool for WriteFileTool {
    type Input = WriteFileInput;
    type Output = WriteFileOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "Creates or overwrites a file. Use for new files or when updating most content of a file.\n",
            "For smaller updates, prefer to use edit or replace_in_file.\n",
            "ALWAYS provide the contents of the COMPLETE file, especially when overwriting existing files!!\n",
            "If the file to write is large, write it in chunks making use of the 'append' parameter.\n",
            "Always end your turn after using this tool, especially when using 'append'.\n",
            "This avoids hitting an output token limit when replying."
        );
        ToolSpec {
            name: "write_file",
            description,
            parameters_schema: json!({
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
            annotations: Some(json!({
                "readOnlyHint": false,
                "destructiveHint": true,
                "idempotentHint": false
            })),
            supported_scopes: &[
                ToolScope::McpServer,
                ToolScope::Agent,
                ToolScope::AgentWithDiffBlocks,
            ],
            hidden: false,
            title_template: Some("Writing {path}"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
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

        // Load project configuration
        let project_config = context
            .project_manager
            .get_project(&input.project)?
            .ok_or_else(|| anyhow::anyhow!("Project not found: {}", input.project))?;

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

        // Write the file first
        match explorer.write_file(&full_path, &input.content, input.append) {
            Ok(mut full_content) => {
                // If format-on-save applies, run the formatter
                if let Some(command_line) = project_config.format_command_for(&path) {
                    let _ = context
                        .command_executor
                        .execute(&command_line, Some(&explorer.root_dir()))
                        .await;

                    // Regardless of formatter success, try to re-read the file content
                    if let Ok(updated) = explorer.read_file(&full_path) {
                        full_content = updated;
                        // Update the input content to the formatted content so the LLM sees it
                        input.content = full_content.clone();
                    }
                }

                // Update working memory if present
                if let Some(working_memory) = &mut context.working_memory {
                    working_memory.loaded_resources.insert(
                        (input.project.clone(), path.clone()),
                        LoadedResource::File(full_content.clone()),
                    );
                }

                Ok(WriteFileOutput {
                    path,
                    content: input.content.clone(),
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
    use crate::tests::mocks::ToolTestFixture;
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

        // Create test fixture with working memory
        let mut fixture = ToolTestFixture::new().with_working_memory();
        let mut context = fixture.context();

        // Parameters for write_file
        let mut input = WriteFileInput {
            project: "test".to_string(),
            path: "test.txt".to_string(),
            content: "Test content".to_string(),
            append: false,
        };

        // Execute the tool
        let result = write_file_tool.execute(&mut context, &mut input).await?;

        // Check the result
        assert!(result.error.is_none());

        // Verify that the file was added to working memory
        let working_memory = fixture.working_memory().unwrap();
        assert_eq!(working_memory.loaded_resources.len(), 1);

        // Check that the file is in the working memory
        let resource_key = ("test".to_string(), PathBuf::from("test.txt"));
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

        // Create test fixture with existing file and working memory
        let mut fixture = ToolTestFixture::with_files(vec![(
            "test.txt".to_string(),
            "Initial content".to_string(),
        )])
        .with_working_memory();
        let mut context = fixture.context();

        // Parameters for write_file with append=true
        let mut input = WriteFileInput {
            project: "test-project".to_string(),
            path: "test.txt".to_string(),
            content: "Test content".to_string(),
            append: true, // Append mode
        };

        // Execute the tool
        let result = write_file_tool.execute(&mut context, &mut input).await?;

        // Check the result
        assert!(result.error.is_none());

        // Verify that the file WAS added to working memory (we fixed the behavior)
        let working_memory = fixture.working_memory().unwrap();
        assert_eq!(working_memory.loaded_resources.len(), 1);

        // Check that the file is in the working memory
        let resource_key = ("test-project".to_string(), PathBuf::from("test.txt"));
        assert!(working_memory.loaded_resources.contains_key(&resource_key));

        // Check that the content is the combined content (initial + appended)
        if let Some(LoadedResource::File(content)) =
            working_memory.loaded_resources.get(&resource_key)
        {
            assert!(content.contains("Initial content"));
            assert!(content.contains("Test content"));
        } else {
            panic!("Expected file resource in working memory");
        }

        Ok(())
    }
}
