use super::config::ToolsConfig;
use super::render::Render;
use super::result::ToolResult;
use super::spec::ToolSpec;
use crate::permissions::PermissionMediator;
use anyhow::{anyhow, Result};
use command_executor::CommandExecutor;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// Context provided to tools during execution.
///
/// The context itself is application-agnostic: it carries only generic
/// services. Application-specific services (project manager, UI, plan state,
/// sub-agent runner, …) travel in `extensions` as owned handles; tools
/// downcast them to the concrete type their application registered.
pub struct ToolContext<'a> {
    /// Command executor for running shell commands
    pub command_executor: &'a dyn CommandExecutor,
    /// Optional current tool ID for streaming output
    pub tool_id: Option<String>,
    /// Optional permission handler for potentially sensitive operations
    pub permission_handler: Option<&'a dyn PermissionMediator>,
    /// Application-specific services for tools that need more than the fields
    /// above. Tools downcast this to the concrete type they were registered
    /// with, keeping the context itself application-agnostic.
    pub extensions: Option<&'a mut (dyn std::any::Any + Send)>,
}

impl<'a> ToolContext<'a> {
    /// Downcast the application-specific extensions to a concrete type.
    pub fn extension<T: 'static>(&self) -> Option<&T> {
        self.extensions
            .as_deref()
            .and_then(|ext| ext.downcast_ref::<T>())
    }

    /// Downcast the application-specific extensions to a concrete type, mutably.
    pub fn extension_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.extensions
            .as_deref_mut()
            .and_then(|ext| ext.downcast_mut::<T>())
    }
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

    /// Check if this tool is available based on configuration.
    /// Tools that require external API keys or services should override this
    /// to return false when their requirements are not met.
    /// Default implementation returns true (tool is always available).
    fn is_available(&self, _config: &ToolsConfig) -> bool {
        true
    }

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
