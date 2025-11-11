use super::render::Render;
use super::result::ToolResult;
use super::spec::ToolSpec;
use crate::types::{PlanState, WorkingMemory};
use anyhow::{anyhow, Result};
use command_executor::CommandExecutor;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// Context provided to tools during execution
pub struct ToolContext<'a> {
    /// Project manager for accessing files
    pub project_manager: &'a dyn crate::config::ProjectManager,
    /// Command executor for running shell commands
    pub command_executor: &'a dyn CommandExecutor,
    /// Optional working memory (available in WorkingMemoryAgent mode)
    pub working_memory: Option<&'a mut WorkingMemory>,
    /// Optional plan state reference for plan-related tools
    pub plan: Option<&'a mut PlanState>,
    /// Optional UI instance for streaming output
    pub ui: Option<&'a dyn crate::ui::UserInterface>,
    /// Optional current tool ID for streaming output
    pub tool_id: Option<String>,
}

/// Core trait for tools, defining the execution interface
#[async_trait::async_trait]
pub trait Tool: Send + Sync + 'static {
    /// Input type for this tool, must be deserializable from JSON
    type Input: DeserializeOwned + Serialize + Send;

    /// Output type for this tool, must implement Render, ToolResult and Serialize/Deserialize
    type Output: Render + ToolResult + Serialize + for<'de> Deserialize<'de> + Send + Sync;

    /// Get the metadata for this tool
    fn spec(&self) -> ToolSpec;

    /// Execute the tool with the given context and input
    /// The input may be modified during execution (e.g., for format-on-save)
    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output>;

    /// Deserialize a JSON value into this tool's output type
    fn deserialize_output(&self, json: serde_json::Value) -> Result<Self::Output> {
        serde_json::from_value(json).map_err(|e| anyhow!("Failed to deserialize output: {e}"))
    }
}
