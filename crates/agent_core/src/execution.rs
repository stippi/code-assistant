//! Self-describing journal entries for calls without a concrete tool result.
//! Successful/functional-error tool outputs retain their existing codecs.

use serde::{Deserialize, Serialize};
use tools_core::{Render, ResourcesTracker, ToolResult};

pub(crate) const RUNTIME_OUTPUT_CODEC: &str = "__agent_runtime_outcome_v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    NotStarted,
    /// Persisted BEFORE invoking a tool. After interruption we cannot tell
    /// whether its effects happened, including the save/invoke crash window.
    Started,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeToolOutput {
    pub state: ExecutionState,
    pub message: String,
}

impl RuntimeToolOutput {
    pub fn not_started(reason: impl AsRef<str>) -> Self {
        Self {
            state: ExecutionState::NotStarted,
            message: format!("Tool execution has not started. {}", reason.as_ref()),
        }
    }

    pub fn started() -> Self {
        Self {
            state: ExecutionState::Started,
            message: "Tool execution may have started, but its outcome is unknown. Verify the state before retrying any side effects.".into(),
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            state: ExecutionState::Failed,
            message: message.into(),
        }
    }
}

impl Render for RuntimeToolOutput {
    fn status(&self) -> String {
        match self.state {
            ExecutionState::NotStarted => "Not started",
            ExecutionState::Started => "Outcome unknown",
            ExecutionState::Succeeded => "Success",
            ExecutionState::Failed => "Error",
        }
        .into()
    }

    fn render(&self, _: &mut ResourcesTracker) -> String {
        self.message.clone()
    }
}

impl ToolResult for RuntimeToolOutput {
    fn is_success(&self) -> bool {
        self.state == ExecutionState::Succeeded
    }
}
