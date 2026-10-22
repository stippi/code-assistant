use crate::tools::ToolServicesAccess;
use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolSpec, capabilities,
};
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Input parameters for the spawn_agent tool.
#[derive(Deserialize, Serialize, Clone, PartialEq)]
pub struct SpawnAgentInput {
    /// The instructions to give to the sub-agent.
    pub instructions: String,
    /// If true, instruct the sub-agent to include file references with line ranges.
    #[serde(default, skip_serializing_if = "is_false")]
    pub require_file_references: bool,
    /// If true, return right away; the result arrives as a message later.
    #[serde(default, skip_serializing_if = "is_false")]
    pub background: bool,
}

fn is_false(b: &bool) -> bool {
    !b
}

/// Output from the spawn_agent tool.
#[derive(Serialize, Deserialize, Default)]
pub struct SpawnAgentOutput {
    /// The final answer from the sub-agent (plain text for LLM context).
    pub answer: String,
    /// Error message if the sub-agent failed or was cancelled.
    pub error: Option<String>,
    /// JSON output for UI display (includes tools list + response for custom renderer).
    /// This is separate from `answer` because the UI needs structured data while
    /// the LLM needs plain text. Persisted so sessions can restore the custom UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_output: Option<String>,
    /// The sub-agent's short per-session id (`#3`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<u64>,
    /// Set while a background sub-agent runs: the process that started it.
    /// A background agent dies with its process, so one started by another
    /// process was lost. Cleared when its result was delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running_in: Option<String>,
}

impl SpawnAgentOutput {
    /// The tool result of a sub-agent started in the background. `answer` is
    /// what the model saw, and stays so: the result arrives as a message.
    pub fn background_started(agent_id: u64, ui_output: String) -> Self {
        Self {
            answer: format!(
                "Sub-agent #{agent_id} started in the background. Its result will arrive as a message starting with `[sub-agent #{agent_id}]`. Continue with other work, or end your turn to wait for it."
            ),
            error: None,
            ui_output: Some(ui_output),
            agent_id: Some(agent_id),
            running_in: Some(crate::session::wakeup::process_instance().to_string()),
        }
    }

    /// A background agent this process cannot deliver anymore.
    fn lost(&self) -> bool {
        self.running_in
            .as_deref()
            .is_some_and(|p| p != crate::session::wakeup::process_instance())
    }
}

impl Render for SpawnAgentOutput {
    fn status(&self) -> String {
        if let Some(e) = &self.error {
            format!("Sub-agent error: {e}")
        } else {
            "Sub-agent completed".to_string()
        }
    }

    /// Returns plain text for LLM context
    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        if let Some(e) = &self.error {
            return format!("Sub-agent error: {e}");
        }
        self.answer.clone()
    }

    /// Returns JSON for UI display (custom renderer)
    fn render_for_ui(&self, tracker: &mut ResourcesTracker) -> String {
        if self.lost() {
            let mut output = self
                .ui_output
                .as_deref()
                .and_then(crate::agent::sub_agent::SubAgentOutput::from_json)
                .unwrap_or_default();
            output.activity = Some(crate::agent::sub_agent::SubAgentActivity::Failed);
            output.error = Some("The app quit while this background sub-agent was running.".into());
            return output.to_json();
        }
        // Use structured JSON output if available, otherwise fall back to plain text
        self.ui_output
            .clone()
            .unwrap_or_else(|| self.render(tracker))
    }
}

impl ToolResult for SpawnAgentOutput {
    fn is_success(&self) -> bool {
        self.error.is_none()
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
            "Use this for exploratory or repetitive work that shouldn't pollute the main conversation history. ",
            "Never use a sub-agent to just read files you already know you have to read and return their content verbatim, as this is both wasteful and expensive for the user. ",
            "Instead read those files directly yourself. The most effective way of using sub-agents is when you are unsure which files to explore.\n\n",
            "The sub-agent has the same tools as you: it can read, search, edit files and run commands. ",
            "Multiple sub-agents spawned in one response run in parallel. ",
            "In a git project, each sub-agent works in its own git worktree, started from your current files; ",
            "when it finishes, its changes are merged into your working tree and the result says which files changed. ",
            "If its changes conflict with edits made meanwhile, nothing is applied and the result names a branch holding them: ",
            "you then resolve the conflict yourself. Give parallel sub-agents separate parts of the code to keep conflicts rare.\n\n",
            "Set `background: true` to not wait for a sub-agent: the tool returns right away and the ",
            "sub-agent's answer arrives later as a message starting with `[sub-agent #<id>]`. ",
            "Meanwhile continue with other work, or end your turn if there is nothing else to do: ",
            "the arriving result starts a new turn."
        );

        ToolSpec {
            name: "spawn_agent".into(),
            description: description.into(),
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
                    "background": {
                        "type": "boolean",
                        "default": false,
                        "description": "If true, don't wait for the sub-agent: its answer arrives later as a message."
                    }
                },
                "required": ["instructions"]
            }),
            annotations: None,
            // Exclude sub-agent scopes to prevent nesting
            capabilities: ToolSpec::capabilities(&[
                capabilities::SCOPE_AGENT,
                capabilities::SCOPE_AGENT_DIFF,
            ]),
            multiline_params: &[],
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
        let sub_agent_runner = context.sub_agent_runner().ok_or_else(|| {
            anyhow!("Sub-agent runner not available. This tool requires the agent to be configured with sub-agent support.")
        })?;
        // Get tool_id for progress streaming
        let tool_id = context
            .tool_id
            .clone()
            .ok_or_else(|| anyhow!("Tool ID not available"))?;

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

        if input.background {
            let started = sub_agent_runner
                .start_background(&tool_id, final_instructions, input.require_file_references)
                .await?;
            return Ok(SpawnAgentOutput::background_started(
                started.agent_id,
                started.ui_output,
            ));
        }

        // Run the sub-agent
        let result = sub_agent_runner
            .run(&tool_id, final_instructions, input.require_file_references)
            .await;

        match result {
            Ok(sub_result) => Ok(SpawnAgentOutput {
                agent_id: agent_id_of(&sub_result.ui_output),
                answer: sub_result.answer,
                error: None,
                ui_output: Some(sub_result.ui_output),
                running_in: None,
            }),
            Err(e) => {
                let ui_output = match e.downcast_ref::<crate::agent::sub_agent::SubAgentFailure>() {
                    Some(failure) => failure.ui_output.clone(),
                    None => {
                        // Alternate runners may only return an error. Still
                        // replace the live card with a terminal structured result.
                        let mut output = crate::agent::sub_agent::SubAgentOutput::new();
                        output.activity = Some(crate::agent::sub_agent::SubAgentActivity::Failed);
                        output.error = Some(e.to_string());
                        output.to_json()
                    }
                };
                Ok(SpawnAgentOutput {
                    agent_id: agent_id_of(&ui_output),
                    answer: String::new(),
                    error: Some(e.to_string()),
                    ui_output: Some(ui_output),
                    running_in: None,
                })
            }
        }
    }
}

fn agent_id_of(ui_output: &str) -> Option<u64> {
    crate::agent::sub_agent::SubAgentOutput::from_json(ui_output)?.agent_id
}
