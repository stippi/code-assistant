use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolScope, ToolSpec,
};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;

// Input type for the execute_command tool
#[derive(Deserialize)]
pub struct ExecuteCommandInput {
    pub project: String,
    pub command_line: String,
    pub working_dir: Option<String>,
}

// Output type
#[derive(Serialize, Deserialize)]
pub struct ExecuteCommandOutput {
    #[allow(dead_code)]
    pub project: String,
    pub command_line: String,
    #[allow(dead_code)]
    pub working_dir: Option<PathBuf>,
    pub output: String,
    pub success: bool,
}

// Render implementation for output formatting
impl Render for ExecuteCommandOutput {
    fn status(&self) -> String {
        if self.success {
            format!("Command executed successfully: {}", self.command_line)
        } else {
            format!("Command failed: {}", self.command_line)
        }
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        let mut formatted = String::new();

        // Add execution status
        if self.success {
            formatted.push_str("Status: Success\n");
        } else {
            formatted.push_str("Status: Failed\n");
        }

        // Add command output with formatting
        formatted.push_str(">>>>> OUTPUT:\n");
        formatted.push_str(&self.output);
        formatted.push_str("\n<<<<< END OF OUTPUT");

        formatted
    }
}

// ToolResult implementation
impl ToolResult for ExecuteCommandOutput {
    fn is_success(&self) -> bool {
        self.success
    }
}

// Tool implementation
pub struct ExecuteCommandTool;

#[async_trait::async_trait]
impl Tool for ExecuteCommandTool {
    type Input = ExecuteCommandInput;
    type Output = ExecuteCommandOutput;

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "execute_command",
            description: "Execute a command line within a specified project",
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Name of the project context for the command"
                    },
                    "command_line": {
                        "type": "string",
                        "description": "The complete command to execute"
                    },
                    "working_dir": {
                        "type": "string",
                        "description": "Optional: working directory for the command (relative to project root)"
                    }
                },
                "required": ["project", "command_line"]
            }),
            annotations: Some(json!({
                "readOnlyHint": false,
                "idempotentHint": false
            })),
            supported_scopes: &[ToolScope::McpServer, ToolScope::Agent],
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

        // Create a PathBuf for the working directory if provided
        let working_dir_path = input.working_dir.as_ref().map(PathBuf::from);

        // Check if working directory is absolute and handle it properly
        if let Some(dir) = &working_dir_path {
            if dir.is_absolute() {
                return Err(anyhow!(
                    "Working directory must be relative to project root"
                ));
            }
        }

        // Prepare effective working directory
        let effective_working_dir = working_dir_path
            .as_ref()
            .map(|dir| explorer.root_dir().join(dir))
            .unwrap_or_else(|| explorer.root_dir());

        // Execute the command using the command executor from context
        let result = context
            .command_executor
            .execute(&input.command_line, Some(&effective_working_dir))
            .await?;

        Ok(ExecuteCommandOutput {
            project: input.project,
            command_line: input.command_line,
            working_dir: working_dir_path,
            output: result.output,
            success: result.success,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mocks::{
        create_command_executor_mock, create_explorer_mock, create_failed_command_executor_mock,
        MockProjectManager,
    };

    #[tokio::test]
    async fn test_execute_command_output_rendering() {
        // Create output with test data
        let output = ExecuteCommandOutput {
            project: "test-project".to_string(),
            command_line: "ls -la".to_string(),
            working_dir: Some(PathBuf::from("src")),
            output: "file1.rs\nfile2.rs".to_string(),
            success: true,
        };

        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        // Verify rendering
        assert!(rendered.contains("Status: Success"));
        assert!(rendered.contains("file1.rs\nfile2.rs"));
    }

    #[tokio::test]
    async fn test_execute_command_failure_rendering() {
        // Create output with failed command data
        let output = ExecuteCommandOutput {
            project: "test-project".to_string(),
            command_line: "rm -rf /tmp/nonexistent".to_string(),
            working_dir: None,
            output: "rm: cannot remove '/tmp/nonexistent': No such file or directory".to_string(),
            success: false,
        };

        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        // Verify rendering for failed command
        assert!(rendered.contains("Status: Failed"));
        assert!(rendered.contains("cannot remove"));
    }

    #[tokio::test]
    async fn test_execute_command_success() -> Result<()> {
        // Create test project manager
        let test_explorer = create_explorer_mock();

        // Create test command executor with predefined response
        let test_cmd_executor = create_command_executor_mock();

        // Setup the project manager with test explorer
        let mock_project_manager = MockProjectManager::default().with_project(
            "test-project",
            PathBuf::from("./root"),
            Box::new(test_explorer),
        );

        // Create tool context with both project manager and command executor
        let mut context = ToolContext {
            project_manager: &mock_project_manager,
            command_executor: &test_cmd_executor,
            working_memory: None,
        };

        // Create input
        let input = ExecuteCommandInput {
            project: "test-project".to_string(),
            command_line: "ls -la".to_string(),
            working_dir: Some("src".to_string()),
        };

        // Execute tool
        let tool = ExecuteCommandTool;
        let result = tool.execute(&mut context, input).await?;

        // Verify result
        assert_eq!(result.command_line, "ls -la");
        assert_eq!(result.output, "Command output"); // Match expected output from mock
        assert!(result.success);

        // Verify command was executed with correct parameters
        let commands = test_cmd_executor.get_captured_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].command_line, "ls -la");
        assert_eq!(commands[0].working_dir, Some(PathBuf::from("./root/src")));

        Ok(())
    }

    #[tokio::test]
    async fn test_execute_command_failure() -> Result<()> {
        // Create test project manager with explorer
        let test_explorer = create_explorer_mock();
        let mock_project_manager = MockProjectManager::default().with_project(
            "test-project",
            PathBuf::from("./root"),
            Box::new(test_explorer),
        );

        // Create test command executor that returns failure
        let test_cmd_executor = create_failed_command_executor_mock();

        // Create tool context with project manager and failing command executor
        let mut context = ToolContext {
            project_manager: &mock_project_manager,
            command_executor: &test_cmd_executor,
            working_memory: None,
        };

        // Create input
        let input = ExecuteCommandInput {
            project: "test-project".to_string(),
            command_line: "rm -rf /tmp/nonexistent".to_string(),
            working_dir: None,
        };

        // Execute tool
        let tool = ExecuteCommandTool;
        let result = tool.execute(&mut context, input).await?;

        // Verify result shows failure
        assert_eq!(result.command_line, "rm -rf /tmp/nonexistent");
        assert_eq!(result.output, "Command failed: permission denied");
        assert!(!result.success);

        // Verify command was executed
        let commands = test_cmd_executor.get_captured_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].command_line, "rm -rf /tmp/nonexistent");
        assert_eq!(commands[0].working_dir, Some(PathBuf::from("./root")));

        Ok(())
    }
}
