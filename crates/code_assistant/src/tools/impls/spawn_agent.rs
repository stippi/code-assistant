use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolScope, ToolSpec,
};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Input parameters for the spawn_agent tool.
#[derive(Deserialize, Serialize, Clone, PartialEq)]
pub struct SpawnAgentInput {
    /// The instructions to give to the sub-agent.
    pub instructions: String,
    /// If true, instruct the sub-agent to include file references with line ranges.
    #[serde(default)]
    pub require_file_references: bool,
    /// The mode for the sub-agent: "read_only" or "default".
    #[serde(default = "default_mode")]
    pub mode: String,
}

fn default_mode() -> String {
    "read_only".to_string()
}

/// Output from the spawn_agent tool.
#[derive(Serialize, Deserialize)]
pub struct SpawnAgentOutput {
    /// The final answer from the sub-agent (plain text for LLM context).
    pub answer: String,
    /// Whether the sub-agent was cancelled.
    pub cancelled: bool,
    /// Error message if the sub-agent failed.
    pub error: Option<String>,
    /// JSON output for UI display (includes tools list + response for custom renderer).
    /// This is separate from `answer` because the UI needs structured data while
    /// the LLM needs plain text.
    #[serde(skip)]
    pub ui_output: Option<String>,
}

impl Render for SpawnAgentOutput {
    fn status(&self) -> String {
        if let Some(e) = &self.error {
            format!("Sub-agent failed: {e}")
        } else if self.cancelled {
            "Sub-agent cancelled by user".to_string()
        } else {
            "Sub-agent completed".to_string()
        }
    }

    /// Returns plain text for LLM context
    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        if let Some(e) = &self.error {
            return format!("Sub-agent failed: {e}");
        }
        if self.cancelled {
            return "Sub-agent cancelled by user.".to_string();
        }
        self.answer.clone()
    }

    /// Returns JSON for UI display (custom renderer)
    fn render_for_ui(&self, tracker: &mut ResourcesTracker) -> String {
        // Use structured JSON output if available, otherwise fall back to plain text
        self.ui_output
            .clone()
            .unwrap_or_else(|| self.render(tracker))
    }
}

impl ToolResult for SpawnAgentOutput {
    fn is_success(&self) -> bool {
        self.error.is_none() && !self.cancelled
    }
}

/// The spawn_agent tool launches a sub-agent with isolated context.
pub struct SpawnAgentTool;

#[async_trait::async_trait]
impl Tool for SpawnAgentTool {
    type Input = SpawnAgentInput;
    type Output = SpawnAgentOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "Spawns a sub-agent to execute a task with isolated context/history. ",
            "The sub-agent runs independently and only the final answer is returned. ",
            "Use this for exploratory or repetitive work that shouldn't pollute the main conversation history.\n\n",
            "The sub-agent has access to read-only tools by default (file reading, searching, web access). ",
            "Progress is streamed as the sub-agent works."
        );

        ToolSpec {
            name: "spawn_agent",
            description,
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "instructions": {
                        "type": "string",
                        "description": "The instructions/task to give to the sub-agent"
                    },
                    "require_file_references": {
                        "type": "boolean",
                        "default": false,
                        "description": "If true, the sub-agent will be instructed to include exact file references with line ranges (e.g. `path/to/file.rs:10-20`)"
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["read_only", "default"],
                        "default": "read_only",
                        "description": "The mode for the sub-agent. 'read_only' restricts to read-only tools. 'default' allows broader tools (reserved for future use)."
                    }
                },
                "required": ["instructions"]
            }),
            annotations: None,
            // Exclude sub-agent scopes to prevent nesting
            supported_scopes: &[ToolScope::Agent, ToolScope::AgentWithDiffBlocks],
            hidden: false,
            title_template: Some("Running sub-agent"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        // Get the sub-agent runner from context
        let sub_agent_runner = context.sub_agent_runner.ok_or_else(|| {
            anyhow!("Sub-agent runner not available. This tool requires the agent to be configured with sub-agent support.")
        })?;
        // Get tool_id for progress streaming
        let tool_id = context
            .tool_id
            .clone()
            .ok_or_else(|| anyhow!("Tool ID not available"))?;

        // Determine tool scope based on mode
        let tool_scope = match input.mode.as_str() {
            "read_only" => ToolScope::SubAgentReadOnly,
            "default" => ToolScope::SubAgentDefault,
            _ => ToolScope::SubAgentReadOnly, // Default to read-only for safety
        };

        // Build final instructions
        let mut final_instructions = input.instructions.clone();
        if input.require_file_references {
            final_instructions.push_str(
                "\n\n---\n\
                IMPORTANT: When referencing code or files in your answer, always include exact file paths with line ranges.\n\
                Use the format: `path/to/file.rs:10-20` for ranges or `path/to/file.rs:15` for single lines.\n\
                This is required for your response to be considered complete.",
            );
        }

        // Run the sub-agent
        let result = sub_agent_runner
            .run(
                &tool_id,
                final_instructions,
                tool_scope,
                input.require_file_references,
            )
            .await;

        match result {
            Ok(sub_result) => {
                let cancelled = sub_result.answer == "Sub-agent cancelled by user.";
                Ok(SpawnAgentOutput {
                    answer: sub_result.answer,
                    cancelled,
                    error: None,
                    ui_output: Some(sub_result.ui_output),
                })
            }
            Err(e) => Ok(SpawnAgentOutput {
                answer: String::new(),
                cancelled: false,
                error: Some(e.to_string()),
                ui_output: None,
            }),
        }
    }
}
