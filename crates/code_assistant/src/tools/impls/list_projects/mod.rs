use crate::tools::core::{Render, ResourcesTracker, Tool, ToolContext, ToolMode, ToolSpec};
use crate::types::Project;
use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;

// Input type (empty for this tool)
#[derive(Deserialize)]
pub struct ListProjectsInput {}

// Output type
pub struct ListProjectsOutput {
    pub projects: HashMap<String, Project>,
}

// Render implementation for output formatting
impl Render for ListProjectsOutput {
    fn status(&self) -> String {
        if self.projects.is_empty() {
            "No projects available".to_string()
        } else {
            format!("Found {} project(s)", self.projects.len())
        }
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        if self.projects.is_empty() {
            return "No projects available".to_string();
        }

        let mut output = String::from("Available projects:\n");
        for (name, _) in &self.projects {
            output.push_str(&format!("- {}\n", name));
        }

        output
    }
}

// The actual tool implementation
pub struct ListProjectsTool;

#[async_trait::async_trait]
impl Tool for ListProjectsTool {
    type Input = ListProjectsInput;
    type Output = ListProjectsOutput;

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_projects",
            description: include_str!("description.md"),
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
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
        _input: Self::Input,
    ) -> Result<Self::Output> {
        // Load projects using the ProjectManager from the context
        let projects = context.project_manager.get_projects()?;

        Ok(ListProjectsOutput { projects })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_list_projects_renders_correctly() {
        // Create sample data
        let mut projects = HashMap::new();
        projects.insert(
            "test-project".to_string(),
            Project {
                path: std::path::PathBuf::from("/path/to/test-project"),
            },
        );

        let output = ListProjectsOutput { projects };
        let mut tracker = ResourcesTracker::new();

        // Test rendering
        let rendered = output.render(&mut tracker);
        assert!(rendered.contains("Available projects:"));
        assert!(rendered.contains("- test-project"));
    }
}
