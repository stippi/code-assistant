use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolScope, ToolSpec,
};
use crate::types::{FileReplacement, LoadedResource};
use crate::utils::FileUpdaterError;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;

// Input type for the edit tool
#[derive(Deserialize, Serialize)]
pub struct EditInput {
    pub project: String,
    pub path: String,
    pub old_text: String,
    pub new_text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace_all: bool,
}

// Output type
#[derive(Serialize, Deserialize)]
pub struct EditOutput {
    pub project: String,
    pub path: PathBuf,
    pub error: Option<FileUpdaterError>,
}

// Render implementation for output formatting
impl Render for EditOutput {
    fn status(&self) -> String {
        if self.error.is_none() {
            format!("Successfully edited file: {}", self.path.display())
        } else {
            format!("Failed to edit file: {}", self.path.display())
        }
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        if let Some(error) = &self.error {
            match error {
                FileUpdaterError::SearchBlockNotFound(_, _) => {
                    "Could not find old_text. Make sure it matches exactly what's in the file."
                        .to_string()
                }
                FileUpdaterError::MultipleMatches(count, _, _) => {
                    format!(
                        "Found {count} occurrences of old_text\nIt must match exactly one location. Try enlarging old_text to make it unique or use replace_all to replace all occurrences."
                    )
                }
                FileUpdaterError::OverlappingMatches(index1, index2) => {
                    format!("Overlapping replacements detected (blocks {index1} and {index2})")
                }
                FileUpdaterError::AdjacentMatches(index1, index2) => {
                    format!("Adjacent replacements detected (blocks {index1} and {index2})")
                }
                FileUpdaterError::Other(msg) => {
                    format!("Failed to edit file '{}': {}", self.path.display(), msg)
                }
            }
        } else {
            format!("Successfully edited file '{}'", self.path.display())
        }
    }
}

// ToolResult implementation
impl ToolResult for EditOutput {
    fn is_success(&self) -> bool {
        self.error.is_none()
    }
}

// Tool implementation
pub struct EditTool;

#[async_trait::async_trait]
impl Tool for EditTool {
    type Input = EditInput;
    type Output = EditOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "Edit a file by replacing specific text content. ",
            "This tool finds the exact text specified in old_text and replaces it with new_text. ",
            "By default, the old_text must match exactly one location in the file. ",
            "Set replace_all to true to replace all occurrences of the pattern.",
        );
        ToolSpec {
            name: "edit",
            description,
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Name of the project containing the file"
                    },
                    "path": {
                        "type": "string",
                        "description": "Path to the file to modify (relative to project root)"
                    },
                    "old_text": {
                        "type": "string",
                        "description": "The exact text content to find and replace. This must match exactly what appears in the file, including whitespace and line breaks. The search is case-sensitive and whitespace-sensitive."
                    },
                    "new_text": {
                        "type": "string",
                        "description": "The text content to replace the old_text with. Can be empty to delete the old_text. Maintains the same indentation and formatting as needed."
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "Optional. If true, replace all occurrences of old_text. If false or omitted, old_text must match exactly one location (default: false)."
                    }
                },
                "required": ["project", "path", "old_text", "new_text"]
            }),
            annotations: Some(json!({
                "readOnlyHint": false,
                "destructiveHint": true
            })),
            supported_scopes: &[ToolScope::McpServer, ToolScope::Agent],
            hidden: false,
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

        // Get project configuration for format-on-save
        let project_config = context
            .project_manager
            .get_project(&input.project)?
            .ok_or_else(|| anyhow!("Project not found: {}", input.project))?;

        // Check for absolute path
        let path = PathBuf::from(&input.path);
        if path.is_absolute() {
            return Ok(EditOutput {
                project: input.project.clone(),
                path,
                error: Some(FileUpdaterError::Other(
                    "Absolute paths are not allowed".to_string(),
                )),
            });
        }

        // Join with root_dir to get full path
        let full_path = explorer.root_dir().join(&path);

        // Create a FileReplacement from the input
        let replacement = FileReplacement {
            search: input.old_text.clone(),
            replace: input.new_text.clone(),
            replace_all: input.replace_all,
        };

        // Apply with or without formatting, based on project configuration
        let format_result = if let Some(format_command) = project_config.format_command_for(&path) {
            explorer
                .apply_replacements_with_formatting(
                    &full_path,
                    &[replacement.clone()],
                    &format_command,
                    context.command_executor,
                )
                .await
        } else {
            match explorer.apply_replacements(&full_path, &[replacement]) {
                Ok(content) => Ok((content, None)),
                Err(e) => Err(e),
            }
        };

        match format_result {
            Ok((new_content, updated_replacements)) => {
                // If formatting updated the replacement parameters, update our input
                if let Some(updated) = updated_replacements {
                    if let Some(updated_replacement) = updated.first() {
                        input.old_text = updated_replacement.search.clone();
                        input.new_text = updated_replacement.replace.clone();
                        input.replace_all = updated_replacement.replace_all;
                    }
                }

                // If we have a working memory reference, update it with the modified file
                if let Some(working_memory) = &mut context.working_memory {
                    // Add the file with new content to working memory
                    working_memory.loaded_resources.insert(
                        (input.project.clone(), path.clone()),
                        LoadedResource::File(new_content.clone()),
                    );
                }

                Ok(EditOutput {
                    project: input.project.clone(),
                    path,
                    error: None,
                })
            }
            Err(e) => {
                // Extract FileUpdaterError if present
                let error = if let Some(file_err) = e.downcast_ref::<FileUpdaterError>() {
                    file_err.clone()
                } else {
                    FileUpdaterError::Other(e.to_string())
                };

                Ok(EditOutput {
                    project: input.project.clone(),
                    path,
                    error: Some(error),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mocks::{MockCommandExecutor, MockExplorer, MockProjectManager};
    use crate::types::WorkingMemory;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_edit_output_rendering() {
        // Success case
        let output = EditOutput {
            project: "test-project".to_string(),
            path: PathBuf::from("src/test.rs"),
            error: None,
        };

        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);
        assert!(rendered.contains("Successfully edited file"));
        assert!(rendered.contains("src/test.rs"));

        // Error case with text not found
        let output_error = EditOutput {
            project: "test-project".to_string(),
            path: PathBuf::from("src/test.rs"),
            error: Some(FileUpdaterError::SearchBlockNotFound(
                0,
                "missing content".to_string(),
            )),
        };

        let rendered_error = output_error.render(&mut tracker);
        assert!(rendered_error.contains("Could not find old_text"));
        assert!(rendered_error.contains("matches exactly"));

        // Error case with multiple matches
        let output_multiple = EditOutput {
            project: "test-project".to_string(),
            path: PathBuf::from("src/test.rs"),
            error: Some(FileUpdaterError::MultipleMatches(
                3,
                0,
                "common pattern".to_string(),
            )),
        };

        let rendered_multiple = output_multiple.render(&mut tracker);
        assert!(rendered_multiple.contains("Found 3 occurrences"));
        assert!(rendered_multiple.contains("Try enlarging old_text"));
        assert!(rendered_multiple.contains("make it unique"));
    }

    #[tokio::test]
    async fn test_edit_basic_replacement() -> Result<()> {
        // Create a mock project manager and setup test files
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("./root/test.rs"),
            "fn original() {\n    println!(\"Original\");\n}".to_string(),
        );

        let explorer = MockExplorer::new(files, None);

        let project_manager = Box::new(MockProjectManager::default().with_project_path(
            "test-project",
            PathBuf::from("./root"),
            Box::new(explorer),
        ));

        // Create a command executor
        let command_executor = Box::new(MockCommandExecutor::new(vec![]));

        // Create working memory
        let mut working_memory = WorkingMemory::default();

        // Create a tool context with working memory
        let mut context = ToolContext {
            project_manager: project_manager.as_ref(),
            command_executor: command_executor.as_ref(),
            working_memory: Some(&mut working_memory),
        };

        // Create input for a valid replacement
        let mut input = EditInput {
            project: "test-project".to_string(),
            path: "test.rs".to_string(),
            old_text: "fn original() {\n    println!(\"Original\");\n}".to_string(),
            new_text: "fn renamed() {\n    println!(\"Updated\");\n}".to_string(),
            replace_all: false,
        };

        // Execute the tool
        let tool = EditTool;
        let result = tool.execute(&mut context, &mut input).await?;

        // Verify the result
        assert!(result.error.is_none());

        // Verify that working memory was updated
        assert_eq!(working_memory.loaded_resources.len(), 1);

        // Verify the content in working memory
        let key = ("test-project".to_string(), PathBuf::from("test.rs"));
        if let Some(LoadedResource::File(content)) = working_memory.loaded_resources.get(&key) {
            assert!(content.contains("fn renamed()"));
            assert!(content.contains("println!(\"Updated\")"));
        } else {
            panic!("File not found in working memory or wrong resource type");
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_edit_replace_all() -> Result<()> {
        // Create a mock project manager with test files
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("./root/test.js"),
            "console.log('test1');\nconsole.log('test2');\nconsole.log('test3');".to_string(),
        );

        let explorer = MockExplorer::new(files, None);

        let project_manager = Box::new(MockProjectManager::default().with_project_path(
            "test-project",
            PathBuf::from("./root"),
            Box::new(explorer),
        ));

        // Create a command executor
        let command_executor = Box::new(MockCommandExecutor::new(vec![]));

        // Create working memory
        let mut working_memory = WorkingMemory::default();

        // Create a tool context with working memory
        let mut context = ToolContext {
            project_manager: project_manager.as_ref(),
            command_executor: command_executor.as_ref(),
            working_memory: Some(&mut working_memory),
        };

        // Create input for replace all
        let mut input = EditInput {
            project: "test-project".to_string(),
            path: "test.js".to_string(),
            old_text: "console.log(".to_string(),
            new_text: "logger.debug(".to_string(),
            replace_all: true,
        };

        // Execute the tool
        let tool = EditTool;
        let result = tool.execute(&mut context, &mut input).await?;

        // Verify the result
        assert!(result.error.is_none());

        // Verify the content in working memory
        let key = ("test-project".to_string(), PathBuf::from("test.js"));
        if let Some(LoadedResource::File(content)) = working_memory.loaded_resources.get(&key) {
            assert!(content.contains("logger.debug('test1')"));
            assert!(content.contains("logger.debug('test2')"));
            assert!(content.contains("logger.debug('test3')"));
            assert!(!content.contains("console.log"));
        } else {
            panic!("File not found in working memory or wrong resource type");
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_edit_error_handling() -> Result<()> {
        // Create a mock project manager with test files
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("./root/test.rs"),
            "console.log('test');\nconsole.log('test');\nconsole.log('test');".to_string(),
        );

        let explorer = MockExplorer::new(files, None);

        let project_manager = Box::new(MockProjectManager::default().with_project_path(
            "test-project",
            PathBuf::from("./root"),
            Box::new(explorer),
        ));

        // Create a command executor
        let command_executor = Box::new(MockCommandExecutor::new(vec![]));

        // Create a tool context
        let mut context = ToolContext {
            project_manager: project_manager.as_ref(),
            command_executor: command_executor.as_ref(),
            working_memory: None,
        };

        // Test case with multiple matches but replace_all = false
        let mut input_multiple = EditInput {
            project: "test-project".to_string(),
            path: "test.rs".to_string(),
            old_text: "console.log".to_string(),
            new_text: "console.debug".to_string(),
            replace_all: false,
        };

        // Execute the tool - should fail with multiple matches
        let tool = EditTool;
        let result = tool.execute(&mut context, &mut input_multiple).await?;

        // Verify error for multiple matches
        assert!(result.error.is_some());
        if let Some(FileUpdaterError::MultipleMatches(count, _, _)) = result.error {
            assert_eq!(count, 3);
        } else {
            panic!("Expected MultipleMatches error");
        }

        // Test case with missing content
        let mut input_missing = EditInput {
            project: "test-project".to_string(),
            path: "test.rs".to_string(),
            old_text: "non_existent_content".to_string(),
            new_text: "replacement".to_string(),
            replace_all: false,
        };

        // Execute the tool - should fail with content not found
        let result = tool.execute(&mut context, &mut input_missing).await?;

        // Verify error for missing content
        assert!(result.error.is_some());
        match &result.error {
            Some(FileUpdaterError::SearchBlockNotFound(_, _)) => (),
            _ => panic!("Expected SearchBlockNotFound error"),
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_edit_empty_replacement() -> Result<()> {
        // Test deleting content by replacing with empty string
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("./root/test.rs"),
            "fn test() {\n    // TODO: Remove this comment\n    println!(\"Hello\");\n}"
                .to_string(),
        );

        let explorer = MockExplorer::new(files, None);

        let project_manager = Box::new(MockProjectManager::default().with_project_path(
            "test-project",
            PathBuf::from("./root"),
            Box::new(explorer),
        ));

        let command_executor = Box::new(MockCommandExecutor::new(vec![]));
        let mut working_memory = WorkingMemory::default();

        let mut context = ToolContext {
            project_manager: project_manager.as_ref(),
            command_executor: command_executor.as_ref(),
            working_memory: Some(&mut working_memory),
        };

        // Delete the TODO comment
        let mut input = EditInput {
            project: "test-project".to_string(),
            path: "test.rs".to_string(),
            old_text: "    // TODO: Remove this comment\n".to_string(),
            new_text: "".to_string(),
            replace_all: false,
        };

        let tool = EditTool;
        let result = tool.execute(&mut context, &mut input).await?;

        // Verify the result
        assert!(result.error.is_none());

        // Verify the content in working memory
        let key = ("test-project".to_string(), PathBuf::from("test.rs"));
        if let Some(LoadedResource::File(content)) = working_memory.loaded_resources.get(&key) {
            assert!(!content.contains("TODO"));
            assert!(content.contains("fn test() {"));
            assert!(content.contains("println!(\"Hello\");"));
        } else {
            panic!("File not found in working memory or wrong resource type");
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_edit_whitespace_normalization() -> Result<()> {
        // Test that whitespace differences are handled correctly
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("./root/test.rs"),
            "function test() {\r\n  console.log('test');\r\n}".to_string(), // CRLF endings
        );

        let explorer = MockExplorer::new(files, None);

        let project_manager = Box::new(MockProjectManager::default().with_project_path(
            "test-project",
            PathBuf::from("./root"),
            Box::new(explorer),
        ));

        let command_executor = Box::new(MockCommandExecutor::new(vec![]));
        let mut working_memory = WorkingMemory::default();

        let mut context = ToolContext {
            project_manager: project_manager.as_ref(),
            command_executor: command_executor.as_ref(),
            working_memory: Some(&mut working_memory),
        };

        // Use LF endings in search text, should still match CRLF in file
        let mut input = EditInput {
            project: "test-project".to_string(),
            path: "test.rs".to_string(),
            old_text: "function test() {\n  console.log('test');\n}".to_string(), // LF endings
            new_text: "function answer() {\n  return 42;\n}".to_string(),
            replace_all: false,
        };

        let tool = EditTool;
        let result = tool.execute(&mut context, &mut input).await?;

        // Verify the result
        assert!(result.error.is_none());

        // Verify the content in working memory
        let key = ("test-project".to_string(), PathBuf::from("test.rs"));
        if let Some(LoadedResource::File(content)) = working_memory.loaded_resources.get(&key) {
            assert!(content.contains("function answer()"));
            assert!(content.contains("return 42;"));
            assert!(!content.contains("console.log"));
        } else {
            panic!("File not found in working memory or wrong resource type");
        }

        Ok(())
    }
}
