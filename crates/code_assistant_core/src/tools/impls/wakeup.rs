//! `schedule_wakeup` / `cancel_wakeup`: the agent arms a timed continuation
//! of its own session. See `crate::session::wakeup` and
//! `docs/session-wakeups.md`.

use crate::tools::core::{
    capabilities, Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolSpec,
};
use crate::tools::ToolServicesAccess;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Upper bound on the delay: wakeups are in-memory and die with the process,
/// so far-future deadlines are better served by application-level schedulers.
const MAX_DELAY_SECONDS: u64 = 7 * 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScheduleWakeupInput {
    pub delay_seconds: u64,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleWakeupOutput {
    pub wakeup_id: u64,
    pub delay_seconds: u64,
}

impl Render for ScheduleWakeupOutput {
    fn status(&self) -> String {
        format!(
            "Wakeup #{} armed, fires in {}s",
            self.wakeup_id, self.delay_seconds
        )
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        format!(
            "Wakeup #{} armed: this session will be woken with your prompt in {} seconds. \
             It is not persisted — it is lost if the application exits.",
            self.wakeup_id, self.delay_seconds
        )
    }
}

impl ToolResult for ScheduleWakeupOutput {
    fn is_success(&self) -> bool {
        true
    }
}

pub struct ScheduleWakeupTool;

#[async_trait::async_trait]
impl Tool for ScheduleWakeupTool {
    type Input = ScheduleWakeupInput;
    type Output = ScheduleWakeupOutput;

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "schedule_wakeup".into(),
            description: "Schedule a wakeup for this session: after the given delay, a new turn \
                is started with your prompt injected as a message (prefixed '[scheduled wakeup]'). \
                Use it to check back on long-running or external work instead of polling. \
                Wakeups are not persisted: they are lost when the application exits."
                .into(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "delay_seconds": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_DELAY_SECONDS,
                        "description": "Delay until the wakeup fires, in seconds."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The message you want to receive when the wakeup fires — write it for your future self (what to check, why)."
                    }
                },
                "required": ["delay_seconds", "prompt"]
            }),
            annotations: None,
            capabilities: ToolSpec::capabilities(&[
                capabilities::READ_ONLY,
                capabilities::SCOPE_AGENT,
                capabilities::SCOPE_AGENT_DIFF,
            ]),
            multiline_params: &["prompt"],
            hidden: false,
            title_template: Some("Scheduling wakeup in {delay_seconds}s"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        if input.delay_seconds == 0 || input.delay_seconds > MAX_DELAY_SECONDS {
            return Err(anyhow!(
                "delay_seconds must be between 1 and {MAX_DELAY_SECONDS}"
            ));
        }
        let prompt = input.prompt.trim();
        if prompt.is_empty() {
            return Err(anyhow!("prompt must not be empty"));
        }

        let wakeups = context
            .services()
            .wakeups
            .as_ref()
            .ok_or_else(|| anyhow!("Wakeups are unavailable in this context"))?;
        let wakeup_id = wakeups.arm(Duration::from_secs(input.delay_seconds), prompt.to_string());

        Ok(ScheduleWakeupOutput {
            wakeup_id,
            delay_seconds: input.delay_seconds,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CancelWakeupInput {
    pub wakeup_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelWakeupOutput {
    pub wakeup_id: u64,
}

impl Render for CancelWakeupOutput {
    fn status(&self) -> String {
        format!("Wakeup #{} cancelled", self.wakeup_id)
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        format!(
            "Wakeup #{} cancelled (no-op if it had already fired).",
            self.wakeup_id
        )
    }
}

impl ToolResult for CancelWakeupOutput {
    fn is_success(&self) -> bool {
        true
    }
}

pub struct CancelWakeupTool;

#[async_trait::async_trait]
impl Tool for CancelWakeupTool {
    type Input = CancelWakeupInput;
    type Output = CancelWakeupOutput;

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "cancel_wakeup".into(),
            description: "Cancel a wakeup previously armed with schedule_wakeup. \
                A no-op if the wakeup already fired."
                .into(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "wakeup_id": {
                        "type": "integer",
                        "description": "The id returned by schedule_wakeup."
                    }
                },
                "required": ["wakeup_id"]
            }),
            annotations: None,
            capabilities: ToolSpec::capabilities(&[
                capabilities::READ_ONLY,
                capabilities::SCOPE_AGENT,
                capabilities::SCOPE_AGENT_DIFF,
            ]),
            multiline_params: &[],
            hidden: false,
            title_template: Some("Cancelling wakeup #{wakeup_id}"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        let wakeups = context
            .services()
            .wakeups
            .as_ref()
            .ok_or_else(|| anyhow!("Wakeups are unavailable in this context"))?;
        wakeups.cancel(input.wakeup_id);
        Ok(CancelWakeupOutput {
            wakeup_id: input.wakeup_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::ToolTestFixture;

    #[tokio::test]
    async fn schedule_arms_and_reports_id() {
        let tool = ScheduleWakeupTool;
        let mut fixture = ToolTestFixture::new().with_wakeups();
        let mut context = fixture.context();

        let mut input = ScheduleWakeupInput {
            delay_seconds: 60,
            prompt: "check the build".into(),
        };
        let output = tool.execute(&mut context, &mut input).await.unwrap();
        assert!(output.wakeup_id >= 1);
        assert_eq!(output.delay_seconds, 60);
    }

    #[tokio::test]
    async fn schedule_rejects_zero_delay_and_empty_prompt() {
        let tool = ScheduleWakeupTool;
        let mut fixture = ToolTestFixture::new().with_wakeups();
        let mut context = fixture.context();

        let mut input = ScheduleWakeupInput {
            delay_seconds: 0,
            prompt: "x".into(),
        };
        assert!(tool.execute(&mut context, &mut input).await.is_err());

        let mut input = ScheduleWakeupInput {
            delay_seconds: 10,
            prompt: "   ".into(),
        };
        assert!(tool.execute(&mut context, &mut input).await.is_err());
    }

    #[tokio::test]
    async fn tools_error_without_wakeup_context() {
        let tool = ScheduleWakeupTool;
        let mut fixture = ToolTestFixture::new();
        let mut context = fixture.context();
        let mut input = ScheduleWakeupInput {
            delay_seconds: 10,
            prompt: "x".into(),
        };
        let err = tool.execute(&mut context, &mut input).await.unwrap_err();
        assert!(err.to_string().contains("unavailable"));
    }
}
