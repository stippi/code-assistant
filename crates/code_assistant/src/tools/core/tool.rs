use super::render::Render;
use super::result::ToolResult;
use super::spec::ToolSpec;
use crate::types::WorkingMemory;
use anyhow::Result;
use serde::de::DeserializeOwned;

/// Context provided to tools during execution
pub struct ToolContext<'a> {
    /// Project manager for accessing files
    pub project_manager: Box<dyn crate::config::ProjectManager>,
    /// Command executor for running shell commands
    pub command_executor: Box<dyn crate::utils::CommandExecutor>,
    /// Optional working memory (available in WorkingMemoryAgent mode)
    pub working_memory: Option<&'a mut WorkingMemory>,
}

/// Core trait for tools, defining the execution interface
#[async_trait::async_trait]
pub trait Tool: Send + Sync + 'static {
    /// Input type for this tool, must be deserializable from JSON
    type Input: DeserializeOwned + Send;

    /// Output type for this tool, must implement Render and ToolResult
    type Output: Render + ToolResult + Send + Sync;

    /// Get the metadata for this tool
    fn spec(&self) -> ToolSpec;

    /// Execute the tool with the given context and input
    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: Self::Input,
    ) -> Result<Self::Output>;
}
