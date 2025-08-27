use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolScope, ToolSpec,
};
use crate::tools::parse::PathWithLineRange;
use crate::types::LoadedResource;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;

// Input type for the read_files tool
#[derive(Deserialize, Serialize)]
pub struct ReadFilesInput {
    pub project: String,
    pub paths: Vec<String>,
}

// Output type
#[derive(Serialize, Deserialize)]
pub struct ReadFilesOutput {
    pub project: String,
    pub loaded_files: HashMap<PathBuf, String>,
    pub failed_files: Vec<(PathBuf, String)>,
}

// Render implementation for output formatting
impl Render for ReadFilesOutput {
    fn status(&self) -> String {
        if self.failed_files.is_empty() {
            format!("Successfully loaded {} file(s)", self.loaded_files.len())
        } else {
            format!(
                "Loaded {} file(s), failed to load {} file(s)",
                self.loaded_files.len(),
                self.failed_files.len()
            )
        }
    }

    fn render(&self, tracker: &mut ResourcesTracker) -> String {
        let mut formatted = String::new();

        // Handle failed files first
        for (path, error) in &self.failed_files {
            formatted.push_str(&format!(
                "Failed to load '{}' in project '{}': {}\n",
                path.display(),
                self.project,
                error
            ));
        }

        // Format loaded files, checking for redundancy
        if !self.loaded_files.is_empty() {
            formatted.push_str("Successfully loaded the following file(s):\n");

            for (path, content) in &self.loaded_files {
                // Generate a unique resource ID for this file with content hash
                let content_hash = format!("{:x}", md5::compute(content));
                let resource_id =
                    format!("file:{}:{}:{}", self.project, path.display(), content_hash);

                if !tracker.is_rendered(&resource_id) {
                    // This file hasn't been rendered yet
                    formatted.push_str(&format!(
                        ">>>>> FILE: {}\n{}\n<<<<< END FILE\n",
                        path.display(),
                        content
                    ));

                    // Mark as rendered
                    tracker.mark_rendered(resource_id);
                } else {
                    // This file has already been rendered
                    formatted.push_str(&format!(
                        ">>>>> FILE: {} (content shown in another tool invocation)\n<<<<< END FILE\n",
                        path.display()
                    ));
                }
            }
        }

        formatted
    }
}

// ToolResult implementation
impl ToolResult for ReadFilesOutput {
    fn is_success(&self) -> bool {
        !self.loaded_files.is_empty() && self.failed_files.is_empty()
    }
}

// Tool implementation
pub struct ReadFilesTool;

#[async_trait::async_trait]
impl Tool for ReadFilesTool {
    type Input = ReadFilesInput;
    type Output = ReadFilesOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
          "Load files into working memory. You can specify line ranges by appending them to the file path using a colon.\n",
          "\n",
          "Examples:\n",
          "- file.txt - Read the entire file. Prefer this form unless you are absolutely sure you need only a section of the file.\n",
          "- file.txt:10-20 - Read only lines 10 to 20\n",
          "- file.txt:10- - Read from line 10 to the end\n",
          "- file.txt:-20 - Read from the beginning to line 20\n",
          "- file.txt:15 - Read only line 15"
        );
        ToolSpec {
            name: "read_files",
            description,
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Name of the project containing the files"
                    },
                    "paths": {
                        "type": "array",
                        "description": "Paths to the files relative to the project root directory. Can include line ranges using 'file.txt:10-20' syntax.",
                        "items": {
                            "type": "string"
                        }
                    }
                },
                "required": ["project", "paths"]
            }),
            annotations: Some(json!({
                "readOnlyHint": true,
                "idempotentHint": true
            })),
            supported_scopes: &[
                ToolScope::McpServer,
                ToolScope::Agent,
                ToolScope::AgentWithDiffBlocks,
            ],
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

        let mut loaded_files = HashMap::new();
        let mut failed_files = Vec::new();

        // Process each path
        for path_str in input.paths.clone() {
            // Parse the path string to extract line range information
            let parsed_path = match PathWithLineRange::parse(&path_str) {
                Ok(parsed) => parsed,
                Err(e) => {
                    failed_files.push((PathBuf::from(path_str), e.to_string()));
                    continue;
                }
            };

            let path = &parsed_path.path;

            // Check for absolute paths
            if path.is_absolute() {
                failed_files.push((path.clone(), "Absolute paths are not allowed".to_string()));
                continue;
            }

            // Join with root_dir to get full path
            let full_path = explorer.root_dir().join(path);

            // Use either read_file_range or read_file based on whether we have line range info
            let read_result = if parsed_path.start_line.is_some() || parsed_path.end_line.is_some()
            {
                // We have line range information, use read_file_range
                explorer.read_file_range(&full_path, parsed_path.start_line, parsed_path.end_line)
            } else {
                // No line range specified, read the whole file
                explorer.read_file(&full_path)
            };

            match read_result {
                Ok(content) => {
                    loaded_files.insert(PathBuf::from(&path_str), content);
                }
                Err(e) => {
                    failed_files.push((PathBuf::from(&path_str), e.to_string()));
                }
            }
        }

        // If we have a working memory reference, update it with the loaded files
        if let Some(working_memory) = &mut context.working_memory {
            // Store successfully loaded files in working memory
            for (path, content) in &loaded_files {
                // Get the base path without any line range information
                let base_path =
                    if let Ok(parsed) = PathWithLineRange::parse(path.to_str().unwrap_or("")) {
                        parsed.path.clone()
                    } else {
                        path.clone()
                    };
                working_memory.loaded_resources.insert(
                    (input.project.clone(), base_path.clone()),
                    LoadedResource::File(content.clone()),
                );
            }
        }

        Ok(ReadFilesOutput {
            project: input.project.clone(),
            loaded_files,
            failed_files,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mocks::{MockExplorer, MockProjectManager};
    use crate::tools::core::ToolRegistry;
    use crate::types::WorkingMemory;

    #[tokio::test]
    async fn test_read_files_output_rendering() {
        // Create output with some test data
        let mut loaded_files = HashMap::new();
        loaded_files.insert(PathBuf::from("test.txt"), "Test file content".to_string());

        let failed_files = vec![(PathBuf::from("missing.txt"), "File not found".to_string())];

        let output = ReadFilesOutput {
            project: "test-project".to_string(),
            loaded_files,
            failed_files,
        };

        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        // Verify rendering
        assert!(rendered.contains("Failed to load 'missing.txt'"));
        assert!(rendered.contains("File not found"));
        assert!(rendered.contains(">>>>> FILE: test.txt"));
        assert!(rendered.contains("Test file content"));
    }

    #[tokio::test]
    async fn test_read_files_updates_memory() -> Result<()> {
        // Create a tool registry
        let registry = ToolRegistry::global();

        // Get the read_files tool
        let read_files_tool = registry
            .get("read_files")
            .expect("read_files tool should be registered");

        // Create test files
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("./root/test.txt"),
            "Line 1\nLine 2\nLine 3\nLine 4\nLine 5".to_string(),
        );
        files.insert(
            PathBuf::from("./root/test2.txt"),
            "Another file content".to_string(),
        );

        // Create a mock explorer with these files
        let explorer = MockExplorer::new(files, None);

        // Create working memory
        let mut working_memory = WorkingMemory::default();

        // Create a tool context with working memory
        let mut context = ToolContext::<'_> {
            project_manager: &MockProjectManager::default().with_project_path(
                "test-project",
                PathBuf::from("./root"),
                Box::new(explorer),
            ),
            command_executor: &crate::utils::DefaultCommandExecutor,
            working_memory: Some(&mut working_memory),
        };

        // Parameters for read_files
        let mut params = json!({
            "project": "test-project",
            "paths": ["test.txt", "test2.txt"]
        });

        // Execute the tool
        let result = read_files_tool.invoke(&mut context, &mut params).await?;

        // Format the output
        let mut tracker = crate::tools::core::ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);

        // Check the output
        assert!(output.contains("Successfully loaded"));

        // Verify that the files were added to working memory
        assert_eq!(working_memory.loaded_resources.len(), 2);

        // Check that both files are in the working memory
        assert!(working_memory
            .loaded_resources
            .contains_key(&("test-project".to_string(), "test.txt".into())));
        assert!(working_memory
            .loaded_resources
            .contains_key(&("test-project".to_string(), "test2.txt".into())));

        Ok(())
    }
}
