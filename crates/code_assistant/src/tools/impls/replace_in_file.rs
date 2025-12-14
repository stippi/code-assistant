use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolScope, ToolSpec,
};
use crate::tools::parse::parse_search_replace_blocks;
use anyhow::{anyhow, Result};
use fs_explorer::{FileReplacement, FileUpdaterError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;

// Input type for the replace_in_file tool
#[derive(Deserialize, Serialize)]
pub struct ReplaceInFileInput {
    pub project: String,
    pub path: String,
    pub diff: String,
}

// Output type
#[derive(Serialize, Deserialize)]
pub struct ReplaceInFileOutput {
    #[allow(dead_code)]
    pub project: String,
    pub path: PathBuf,
    pub error: Option<FileUpdaterError>,
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
                FileUpdaterError::SearchBlockNotFound(idx, _) => {
                    format!(
                        "Please adjust your SEARCH block with index {idx} to the current contents of the file."
                    )
                }
                FileUpdaterError::MultipleMatches(count, idx, _) => {
                    format!(
                        "Found {count} occurrences of SEARCH block with index {idx}\nA SEARCH block must match exactly one location. Try enlarging the section to replace."
                    )
                }
                FileUpdaterError::OverlappingMatches(index1, index2) => {
                    format!("Overlapping SEARCH blocks detected (blocks {index1} and {index2})")
                }
                FileUpdaterError::AdjacentMatches(index1, index2) => {
                    format!("Adjacent SEARCH blocks detected (blocks {index1} and {index2})")
                }
                FileUpdaterError::Other(msg) => {
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

fn render_diff_from_replacements(replacements: &[FileReplacement]) -> String {
    let mut out = String::new();
    for (i, r) in replacements.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if r.replace_all {
            out.push_str("<<<<<<< SEARCH_ALL\n");
            out.push_str(&r.search);
            out.push_str("\n=======\n");
            out.push_str(&r.replace);
            out.push_str("\n>>>>>>> REPLACE_ALL");
        } else {
            out.push_str("<<<<<<< SEARCH\n");
            out.push_str(&r.search);
            out.push_str("\n=======\n");
            out.push_str(&r.replace);
            out.push_str("\n>>>>>>> REPLACE");
        }
    }
    out
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
                    "diff": {
                        "type": "string",
                        "description": "One or more SEARCH/REPLACE or SEARCH_ALL/REPLACE_ALL blocks following either of these formats:\n<<<<<<< SEARCH\n[exact content to find]\n=======\n[new content to replace with]\n>>>>>>> REPLACE\n\nOR\n\n<<<<<<< SEARCH_ALL\n[content pattern to find]\n=======\n[new content to replace with]\n>>>>>>> REPLACE_ALL\n\nWith SEARCH/REPLACE blocks, the search content must match exactly one location. With SEARCH_ALL/REPLACE_ALL blocks, all occurrences of the pattern will be replaced."
                    }
                },
                "required": ["project", "path", "diff"]
            }),
            annotations: Some(json!({
                "readOnlyHint": false,
                "destructiveHint": true
            })),
            supported_scopes: &[ToolScope::AgentWithDiffBlocks],
            hidden: false,
            title_template: Some("Replacing in {path}"),
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

        // Load project configuration
        let project_config = context
            .project_manager
            .get_project(&input.project)?
            .ok_or_else(|| anyhow!("Project not found: {}", input.project))?;

        // Parse the replacements from the diff
        let replacements = match parse_search_replace_blocks(&input.diff) {
            Ok(replacements) => replacements,
            Err(e) => {
                return Ok(ReplaceInFileOutput {
                    project: input.project.clone(),
                    path: PathBuf::from(&input.path),
                    error: Some(FileUpdaterError::Other(format!(
                        "Failed to parse replacements: {e}"
                    ))),
                });
            }
        };

        // Check for absolute path
        let path = PathBuf::from(&input.path);
        if path.is_absolute() {
            return Ok(ReplaceInFileOutput {
                project: input.project.clone(),
                path,
                error: Some(FileUpdaterError::Other(
                    "Absolute paths are not allowed".to_string(),
                )),
            });
        }

        // Join with root_dir to get full path
        let full_path = explorer.root_dir().join(&path);

        // If format-on-save applies, use format-aware path
        let result = if let Some(command_line) = project_config.format_command_for(&path) {
            explorer
                .apply_replacements_with_formatting(
                    &full_path,
                    &replacements,
                    &command_line,
                    context.command_executor,
                )
                .await
        } else {
            match explorer.apply_replacements(&full_path, &replacements).await {
                Ok(content) => Ok((content, None)),
                Err(e) => Err(e),
            }
        };

        match result {
            Ok((_new_content, updated_replacements)) => {
                // If we have updated replacements (after formatting), update the diff text
                if let Some(updated) = updated_replacements {
                    input.diff = render_diff_from_replacements(&updated);
                }

                // Emit resource event
                if let Some(ui) = context.ui {
                    let _ = ui
                        .send_event(crate::ui::UiEvent::ResourceWritten {
                            project: input.project.clone(),
                            path: path.clone(),
                        })
                        .await;
                }

                Ok(ReplaceInFileOutput {
                    project: input.project.clone(),
                    path,
                    error: None,
                })
            }
            Err(e) => {
                let error = if let Some(file_err) = e.downcast_ref::<FileUpdaterError>() {
                    file_err.clone()
                } else {
                    FileUpdaterError::Other(e.to_string())
                };

                Ok(ReplaceInFileOutput {
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
    use crate::tests::mocks::ToolTestFixture;

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
            error: Some(FileUpdaterError::SearchBlockNotFound(
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
            error: Some(FileUpdaterError::MultipleMatches(
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

    async fn test_replace_in_file_emits_resource_event() -> Result<()> {
        // Create test fixture with UI for event capture
        let mut fixture = ToolTestFixture::with_files(vec![(
            "test.rs".to_string(),
            "fn original() {\n    println!(\"Original\");\n}".to_string(),
        )])
        .with_ui();
        let mut context = fixture.context();

        // Create input for a valid replacement
        let mut input = ReplaceInFileInput {
            project: "test-project".to_string(),
            path: "test.rs".to_string(),
            diff: "<<<<<<< SEARCH\nfn original() {\n    println!(\"Original\");\n}\n=======\nfn renamed() {\n    println!(\"Updated\");\n}\n>>>>>>> REPLACE".to_string(),
        };

        // Execute the tool
        let tool = ReplaceInFileTool;
        let result = tool.execute(&mut context, &mut input).await?;

        // Verify the result
        assert!(result.error.is_none());

        // Drop context to release borrow
        drop(context);

        // Verify that ResourceWritten event was emitted
        let events = fixture.ui().unwrap().events();
        assert!(events.iter().any(|e| matches!(
            e,
            crate::ui::UiEvent::ResourceWritten { project, path }
            if project == "test-project" && path == &PathBuf::from("test.rs")
        )));

        Ok(())
    }

    #[tokio::test]
    async fn test_replace_in_file_error_handling() -> Result<()> {
        // Create test fixture
        let mut fixture = ToolTestFixture::with_files(vec![(
            "test.rs".to_string(),
            "console.log('test');\nconsole.log('test');\nconsole.log('test');".to_string(),
        )]);
        let mut context = fixture.context();

        // Test case with multiple matches
        let mut input_multiple = ReplaceInFileInput {
            project: "test-project".to_string(),
            path: "test.rs".to_string(),
            diff: "<<<<<<< SEARCH\nconsole.log\n=======\nconsole.debug\n>>>>>>> REPLACE"
                .to_string(),
        };

        // Execute the tool - should fail with multiple matches
        let tool = ReplaceInFileTool;
        let result = tool.execute(&mut context, &mut input_multiple).await?;

        // Verify error for multiple matches
        assert!(result.error.is_some());
        if let Some(FileUpdaterError::MultipleMatches(count, _, _)) = result.error {
            assert_eq!(count, 3);
        } else {
            panic!("Expected MultipleMatches error");
        }

        // Test case with missing content
        let mut input_missing = ReplaceInFileInput {
            project: "test-project".to_string(),
            path: "test.rs".to_string(),
            diff: "<<<<<<< SEARCH\nnon_existent_content\n=======\nreplacement\n>>>>>>> REPLACE"
                .to_string(),
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
}
