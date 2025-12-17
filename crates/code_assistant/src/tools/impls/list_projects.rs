use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolScope, ToolSpec,
};
use crate::types::Project;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;

// Input type (empty for this tool)
#[derive(Deserialize, Serialize)]
pub struct ListProjectsInput {}

// Output type
#[derive(Serialize, Deserialize)]
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
        for name in self.projects.keys() {
            output.push_str(&format!("- {name}\n"));
        }

        output
    }
}

// ToolResult implementation
impl ToolResult for ListProjectsOutput {
    fn is_success(&self) -> bool {
        true // Always successful even if no projects are found
    }
}

// The actual tool implementation
pub struct ListProjectsTool;

#[async_trait::async_trait]
impl Tool for ListProjectsTool {
    type Input = ListProjectsInput;
    type Output = ListProjectsOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "List all available projects. ",
            "Use this tool to discover which projects are available for exploration."
        );
        ToolSpec {
            name: "list_projects",
            description,
            parameters_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            annotations: Some(json!({
                "readOnlyHint": true
            })),
            // This tool is only needed in MCP mode where we don't control the system message.
            // The regular code-assistant will insert known projects into the system message.
            supported_scopes: &[ToolScope::McpServer],
            hidden: false,
            title_template: None, // Uses default tool name
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        _input: &mut Self::Input,
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
                format_on_save: None,
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
