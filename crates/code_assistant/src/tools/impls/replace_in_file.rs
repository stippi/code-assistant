use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolMode, ToolResult, ToolSpec,
};
use crate::tools::parse::parse_search_replace_blocks;
use crate::types::LoadedResource;
use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::path::PathBuf;

// Input type for the replace_in_file tool
#[derive(Deserialize)]
pub struct ReplaceInFileInput {
    pub project: String,
    pub path: String,
    pub diff: String,
}

// Output type
pub struct ReplaceInFileOutput {
    #[allow(dead_code)]
    pub project: String,
    pub path: PathBuf,
    pub error: Option<crate::utils::FileUpdaterError>,
}

// Render implementation for output formatting
impl Render for ReplaceInFileOutput {
    fn status(&self) -> String {
        if self.error.is_none() {
            format!(
                "Successfully replaced content in file: {}",
                self.path.display()
            )
        } else {
            format!("Failed to replace content in file: {}", self.path.display())
        }
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        if let Some(error) = &self.error {
            match error {
                crate::utils::FileUpdaterError::SearchBlockNotFound(idx, _) => {
                    format!(
                        "Please adjust your SEARCH block with index {} to the current contents of the file.",
                        idx
                    )
                }
                crate::utils::FileUpdaterError::MultipleMatches(count, idx, _) => {
                    format!(
                        "Found {} occurrences of SEARCH block with index {}\nA SEARCH block must match exactly one location. Try enlarging the section to replace.",
                        count, idx
                    )
                }
                crate::utils::FileUpdaterError::Other(msg) => {
                    format!(
                        "Failed to replace in file '{}': {}",
                        self.path.display(),
                        msg
                    )
                }
            }
        } else {
            format!(
                "Successfully replaced content in file '{}'",
                self.path.display()
            )
        }
    }
}

// ToolResult implementation
impl ToolResult for ReplaceInFileOutput {
    fn is_success(&self) -> bool {
        self.error.is_none()
    }
}

// Tool implementation
pub struct ReplaceInFileTool;

#[async_trait::async_trait]
impl Tool for ReplaceInFileTool {
    type Input = ReplaceInFileInput;
    type Output = ReplaceInFileOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "Replace sections in a file within a specified project using search/replace blocks.\n",
            "By default, each search text must match exactly once in the file, ",
            "but you can use SEARCH_ALL/REPLACE_ALL blocks to replace all occurrences of a pattern.",
        );
        ToolSpec {
            name: "replace_in_file",
            description,
            parameters_schema: serde_json::json!({
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
                    "diff": {
                        "type": "string",
                        "description": "One or more SEARCH/REPLACE or SEARCH_ALL/REPLACE_ALL blocks following either of these formats:\n<<<<<<< SEARCH\n[exact content to find]\n=======\n[new content to replace with]\n>>>>>>> REPLACE\n\nOR\n\n<<<<<<< SEARCH_ALL\n[content pattern to find]\n=======\n[new content to replace with]\n>>>>>>> REPLACE_ALL\n\nWith SEARCH/REPLACE blocks, the search content must match exactly one location. With SEARCH_ALL/REPLACE_ALL blocks, all occurrences of the pattern will be replaced."
                    }
                },
                "required": ["project", "path", "diff"]
            }),
            annotations: Some(serde_json::json!({
                "readOnlyHint": false,
                "destructiveHint": true
            })),
            supported_modes: &[ToolMode::McpServer, ToolMode::MessageHistoryAgent],
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

        // Parse the replacements from the diff
        let replacements = match parse_search_replace_blocks(&input.diff) {
            Ok(replacements) => replacements,
            Err(e) => {
                return Ok(ReplaceInFileOutput {
                    project: input.project,
                    path: PathBuf::from(&input.path),
                    error: Some(crate::utils::FileUpdaterError::Other(format!(
                        "Failed to parse replacements: {}",
                        e
                    ))),
                });
            }
        };

        // Check for absolute path
        let path = PathBuf::from(&input.path);
        if path.is_absolute() {
            return Ok(ReplaceInFileOutput {
                project: input.project,
                path,
                error: Some(crate::utils::FileUpdaterError::Other(
                    "Absolute paths are not allowed".to_string(),
                )),
            });
        }

        // Join with root_dir to get full path
        let full_path = explorer.root_dir().join(&path);

        // Apply the replacements
        match explorer.apply_replacements(&full_path, &replacements) {
            Ok(new_content) => {
                // If we have a working memory reference, update it with the modified file
                if let Some(working_memory) = &mut context.working_memory {
                    // Remove any existing summary since file is changed
                    working_memory
                        .summaries
                        .remove(&(input.project.clone(), path.clone()));

                    // Add the file with new content to working memory
                    working_memory.loaded_resources.insert(
                        (input.project.clone(), path.clone()),
                        LoadedResource::File(new_content.clone()),
                    );
                }

                Ok(ReplaceInFileOutput {
                    project: input.project,
                    path,
                    error: None,
                })
            }
            Err(e) => {
                // Extract FileUpdaterError if present
                let error =
                    if let Some(file_err) = e.downcast_ref::<crate::utils::FileUpdaterError>() {
                        file_err.clone()
                    } else {
                        crate::utils::FileUpdaterError::Other(e.to_string())
                    };

                Ok(ReplaceInFileOutput {
                    project: input.project,
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
    use crate::tools::tests::mocks::MockProjectManager;
    use crate::types::WorkingMemory;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_replace_in_file_output_rendering() {
        // Success case
        let output = ReplaceInFileOutput {
            project: "test-project".to_string(),
            path: PathBuf::from("src/test.rs"),
            error: None,
        };

        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);
        assert!(rendered.contains("Successfully replaced content"));
        assert!(rendered.contains("src/test.rs"));

        // Error case with block not found
        let output_error = ReplaceInFileOutput {
            project: "test-project".to_string(),
            path: PathBuf::from("src/test.rs"),
            error: Some(crate::utils::FileUpdaterError::SearchBlockNotFound(
                0,
                "missing content".to_string(),
            )),
        };

        let rendered_error = output_error.render(&mut tracker);
        assert!(rendered_error.contains("Please adjust your SEARCH block"));

        // Error case with multiple matches
        let output_multiple = ReplaceInFileOutput {
            project: "test-project".to_string(),
            path: PathBuf::from("src/test.rs"),
            error: Some(crate::utils::FileUpdaterError::MultipleMatches(
                3,
                0,
                "common pattern".to_string(),
            )),
        };

        let rendered_multiple = output_multiple.render(&mut tracker);
        assert!(rendered_multiple.contains("Found 3 occurrences"));
        assert!(rendered_multiple.contains("Try enlarging the section"));
    }

    #[tokio::test]
    async fn test_replace_in_file_working_memory_update() -> Result<()> {
        // Create a mock project manager and setup test files
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("./root/test.rs"),
            "fn original() {\n    println!(\"Original\");\n}".to_string(),
        );

        let explorer = crate::tools::tests::mocks::MockExplorer::new(files, None);

        let project_manager = Box::new(MockProjectManager::default().with_project(
            "test-project",
            PathBuf::from("./root"),
            explorer,
        ));

        // Create a command executor
        let command_executor = Box::new(crate::utils::DefaultCommandExecutor);

        // Create working memory
        let mut working_memory = WorkingMemory::default();

        // Create a tool context with working memory
        let mut context = ToolContext {
            project_manager: project_manager.as_ref(),
            command_executor: command_executor.as_ref(),
            working_memory: Some(&mut working_memory),
        };

        // Create input for a valid replacement
        let input = ReplaceInFileInput {
            project: "test-project".to_string(),
            path: "test.rs".to_string(),
            diff: "<<<<<<< SEARCH\nfn original() {\n    println!(\"Original\");\n}\n=======\nfn renamed() {\n    println!(\"Updated\");\n}\n>>>>>>> REPLACE".to_string(),
        };

        // Execute the tool
        let tool = ReplaceInFileTool;
        let result = tool.execute(&mut context, input).await?;

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
    async fn test_replace_in_file_error_handling() -> Result<()> {
        // Create a mock project manager with test files
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("./root/test.rs"),
            "console.log('test');\nconsole.log('test');\nconsole.log('test');".to_string(),
        );

        let explorer = crate::tools::tests::mocks::MockExplorer::new(files, None);

        let project_manager = Box::new(MockProjectManager::default().with_project(
            "test-project",
            PathBuf::from("./root"),
            explorer,
        ));

        // Create a command executor
        let command_executor = Box::new(crate::utils::DefaultCommandExecutor);

        // Create a tool context
        let mut context = ToolContext {
            project_manager: project_manager.as_ref(),
            command_executor: command_executor.as_ref(),
            working_memory: None,
        };

        // Test case with multiple matches
        let input_multiple = ReplaceInFileInput {
            project: "test-project".to_string(),
            path: "test.rs".to_string(),
            diff: "<<<<<<< SEARCH\nconsole.log\n=======\nconsole.debug\n>>>>>>> REPLACE"
                .to_string(),
        };

        // Execute the tool - should fail with multiple matches
        let tool = ReplaceInFileTool;
        let result = tool.execute(&mut context, input_multiple).await?;

        // Verify error for multiple matches
        assert!(result.error.is_some());
        if let Some(crate::utils::FileUpdaterError::MultipleMatches(count, _, _)) = result.error {
            assert_eq!(count, 3);
        } else {
            panic!("Expected MultipleMatches error");
        }

        // Test case with missing content
        let input_missing = ReplaceInFileInput {
            project: "test-project".to_string(),
            path: "test.rs".to_string(),
            diff: "<<<<<<< SEARCH\nnon_existent_content\n=======\nreplacement\n>>>>>>> REPLACE"
                .to_string(),
        };

        // Execute the tool - should fail with content not found
        let result = tool.execute(&mut context, input_missing).await?;

        // Verify error for missing content
        assert!(result.error.is_some());
        match &result.error {
            Some(crate::utils::FileUpdaterError::SearchBlockNotFound(_, _)) => (),
            _ => panic!("Expected SearchBlockNotFound error"),
        }

        Ok(())
    }
}
