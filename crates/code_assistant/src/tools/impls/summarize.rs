use crate::tools::core::{Render, ResourcesTracker, Tool, ToolContext, ToolMode, ToolResult, ToolSpec};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::path::PathBuf;

// Input type for the summarize tool
#[derive(Deserialize)]
pub struct SummarizeInput {
    pub project: String,
    pub path: PathBuf,
    pub summary: String,
}

// Output type
pub struct SummarizeOutput {
    pub project: String,
    pub path: PathBuf,
    pub summary: String,
}

// Render implementation for output formatting
impl Render for SummarizeOutput {
    fn status(&self) -> String {
        format!(
            "Created summary for [{}] {}",
            self.project,
            self.path.display()
        )
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        format!(
            "Created summary for [{}] {}:\n{}",
            self.project,
            self.path.display(),
            self.summary
        )
    }
}

// ToolResult implementation
impl ToolResult for SummarizeOutput {
    fn is_success(&self) -> bool {
        true // Always successful if we got to this point
    }
}

// The tool implementation
pub struct SummarizeTool;

#[async_trait::async_trait]
impl Tool for SummarizeTool {
    type Input = SummarizeInput;
    type Output = SummarizeOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "Replace contents of resources with summaries in working memory.\n",
            "This tool allows you to create a summary for a resource that you have previously loaded, ",
            "reducing memory usage while preserving key information.\n",
            "The summary will replace the full content in working memory, ",
            "making it easier to keep track of important information without keeping all details in memory."
        );
        ToolSpec {
            name: "summarize",
            description,
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Name of the project containing the resource"
                    },
                    "path": {
                        "type": "string",
                        "description": "Path to the resource to summarize"
                    },
                    "summary": {
                        "type": "string",
                        "description": "Summary text to replace the resource content"
                    }
                },
                "required": ["project", "path", "summary"]
            }),
            annotations: None,
            supported_modes: &[ToolMode::WorkingMemoryAgent],
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: Self::Input,
    ) -> Result<Self::Output> {
        // We need working memory for this tool
        let working_memory = context
            .working_memory
            .as_mut()
            .ok_or_else(|| anyhow!("Working memory is required for the summarize tool"))?;

        // Remove from loaded resources
        let key = (input.project.clone(), input.path.clone());

        // Check if the resource exists before removing
        if !working_memory.loaded_resources.contains_key(&key) {
            return Err(anyhow!(
                "Resource [{}] {} not found in working memory",
                input.project,
                input.path.display()
            ));
        }

        // Remove resource from loaded_resources
        working_memory.loaded_resources.remove(&key);

        // Add to summaries
        working_memory.summaries.insert(key, input.summary.clone());

        Ok(SummarizeOutput {
            project: input.project,
            path: input.path,
            summary: input.summary,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::tests::mocks::{MockCommandExecutor, MockProjectManager};
    use crate::types::{LoadedResource, WorkingMemory};

    #[test]
    fn test_rendering() {
        // Create sample output
        let output = SummarizeOutput {
            project: "test-project".to_string(),
            path: PathBuf::from("example.txt"),
            summary: "This is a summary of the file.".to_string(),
        };

        // Test rendering
        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        assert!(rendered.contains("Created summary for [test-project] example.txt"));
        assert!(rendered.contains("This is a summary of the file."));
    }

    #[tokio::test]
    async fn test_execute() {
        // Create working memory and add a resource
        let mut working_memory = WorkingMemory::default();
        let project = "test-project".to_string();
        let path = PathBuf::from("example.txt");
        let content = "This is the original content.".to_string();

        working_memory.add_resource(project.clone(), path.clone(), LoadedResource::File(content));

        // Create context with working memory
        let mut context = ToolContext {
            project_manager: Box::new(MockProjectManager::new()),
            command_executor: Box::new(MockCommandExecutor::new(vec![])),
            working_memory: Some(&mut working_memory),
        };

        // Create input
        let input = SummarizeInput {
            project: project.clone(),
            path: path.clone(),
            summary: "This is a summary.".to_string(),
        };

        // Execute the tool
        let tool = SummarizeTool;
        let result = tool.execute(&mut context, input).await;

        // Check result
        assert!(result.is_ok());

        // Check working memory was updated
        assert!(!working_memory
            .loaded_resources
            .contains_key(&(project.clone(), path.clone())));
        assert!(working_memory.summaries.contains_key(&(project, path)));
    }
}
