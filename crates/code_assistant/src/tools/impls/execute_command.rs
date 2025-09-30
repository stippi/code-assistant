use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolScope, ToolSpec,
};
use crate::ui::streaming::DisplayFragment;
use crate::ui::UserInterface;
use crate::utils::command::StreamingCallback;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;

// Input type for the execute_command tool
#[derive(Deserialize, Serialize)]
pub struct ExecuteCommandInput {
    pub project: String,
    pub command_line: String,
    #[serde(skip_serializing_if = "Option::is_none")]
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

/// Streaming callback implementation for tool output
struct ToolOutputStreamer<'a> {
    ui: &'a dyn UserInterface,
    tool_id: String,
}

impl<'a> StreamingCallback for ToolOutputStreamer<'a> {
    fn on_output_chunk(&self, chunk: &str) -> Result<()> {
        let fragment = DisplayFragment::ToolOutput {
            tool_id: self.tool_id.clone(),
            chunk: chunk.to_string(),
        };

        // Send to UI synchronously (don't spawn a task to avoid lifetime issues)
        let _ = self.ui.display_fragment(&fragment);

        Ok(())
    }
}

// Tool implementation
pub struct ExecuteCommandTool;

#[async_trait::async_trait]
impl Tool for ExecuteCommandTool {
    type Input = ExecuteCommandInput;
    type Output = ExecuteCommandOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "Execute a command line or shell script within a specified project. ",
            "Blocks until the command returns by itself and then provides all output at once. ",
            "Must not be used with commands that would keep running forever, unless combined with a timeout."
        );
        ToolSpec {
            name: "execute_command",
            description,
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Name of the project context for the command/script"
                    },
                    "command_line": {
                        "type": "string",
                        "description": "The complete command or shell script to execute"
                    },
                    "working_dir": {
                        "type": "string",
                        "description": "Optional: working directory (relative to project root)"
                    }
                },
                "required": ["project", "command_line"]
            }),
            annotations: Some(json!({
                "readOnlyHint": false,
                "idempotentHint": false
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

        // Execute the command using streaming
        let result = match (context.ui, &context.tool_id) {
            (Some(ui), Some(tool_id)) => {
                // Create streaming callback for UI output
                let callback = ToolOutputStreamer {
                    ui,
                    tool_id: tool_id.clone(),
                };

                context
                    .command_executor
                    .execute_streaming(
                        &input.command_line,
                        Some(&effective_working_dir),
                        Some(&callback),
                    )
                    .await?
            }
            _ => {
                // No UI available, use regular execution
                context
                    .command_executor
                    .execute_streaming(&input.command_line, Some(&effective_working_dir), None)
                    .await?
            }
        };

        Ok(ExecuteCommandOutput {
            project: input.project.clone(),
            command_line: input.command_line.clone(),
            working_dir: working_dir_path,
            output: result.output,
            success: result.success,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mocks::ToolTestFixture;

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
        // Create test fixture with command executor and UI
        let mut fixture =
            ToolTestFixture::with_command_responses(vec![Ok(crate::utils::CommandOutput {
                success: true,
                output: "Command output".to_string(),
            })])
            .with_ui()
            .with_tool_id("test-tool-1".to_string());
        let mut context = fixture.context();

        // Create input
        let mut input = ExecuteCommandInput {
            project: "test".to_string(),
            command_line: "ls -la".to_string(),
            working_dir: Some("src".to_string()),
        };

        // Execute tool
        let tool = ExecuteCommandTool;
        let result = tool.execute(&mut context, &mut input).await?;

        // Verify result
        assert_eq!(result.command_line, "ls -la");
        assert_eq!(result.output, "Command output"); // Match expected output from mock
        assert!(result.success);

        // Verify command was executed with correct parameters
        let commands = fixture.command_executor().get_captured_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].command_line, "ls -la");
        assert_eq!(commands[0].working_dir, Some(PathBuf::from("./root/src")));

        Ok(())
    }

    #[tokio::test]
    async fn test_execute_command_failure() -> Result<()> {
        // Create test fixture with failing command executor and UI
        let mut fixture =
            ToolTestFixture::with_command_responses(vec![Ok(crate::utils::CommandOutput {
                success: false,
                output: "Command failed: permission denied".to_string(),
            })])
            .with_ui()
            .with_tool_id("test-tool-2".to_string());
        let mut context = fixture.context();

        // Create input
        let mut input = ExecuteCommandInput {
            project: "test".to_string(),
            command_line: "rm -rf /tmp/nonexistent".to_string(),
            working_dir: None,
        };

        // Execute tool
        let tool = ExecuteCommandTool;
        let result = tool.execute(&mut context, &mut input).await?;

        // Verify result shows failure
        assert_eq!(result.command_line, "rm -rf /tmp/nonexistent");
        assert_eq!(result.output, "Command failed: permission denied");
        assert!(!result.success);

        // Verify command was executed
        let commands = fixture.command_executor().get_captured_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].command_line, "rm -rf /tmp/nonexistent");
        assert_eq!(commands[0].working_dir, Some(PathBuf::from("./root")));

        Ok(())
    }

    #[tokio::test]
    async fn test_execute_command_streaming() -> Result<()> {
        use crate::utils::CommandOutput;

        // Create test fixture with multi-line output and UI for streaming
        let mut fixture = ToolTestFixture::with_command_responses(vec![Ok(CommandOutput {
            success: true,
            output: "Line 1\nLine 2\nLine 3\n".to_string(),
        })])
        .with_ui()
        .with_tool_id("test-streaming-tool".to_string());
        let mut context = fixture.context();

        // Create input
        let mut input = ExecuteCommandInput {
            project: "test".to_string(),
            command_line: "echo 'test'".to_string(),
            working_dir: None,
        };

        // Execute tool
        let tool = ExecuteCommandTool;
        let result = tool.execute(&mut context, &mut input).await?;

        // Verify result
        assert!(result.success);
        assert_eq!(result.output, "Line 1\nLine 2\nLine 3\n");

        // Verify streaming output was captured
        let streaming_output = fixture.ui().unwrap().get_streaming_output();
        assert!(
            !streaming_output.is_empty(),
            "Should have received streaming output"
        );

        // The streaming output should contain the individual lines
        println!("Streaming output received: {streaming_output:?}");

        Ok(())
    }
}
