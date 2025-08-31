use super::render::Render;
use super::result::ToolResult;
use super::spec::ToolSpec;
use super::tool::{Tool, ToolContext};
use crate::types::ToolError;
use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Type-erased tool output that can be rendered and determined for success
pub trait AnyOutput: Send + Sync {
    /// Get a reference to the output as a Render trait object
    fn as_render(&self) -> &dyn Render;

    /// Determine if the tool execution was successful
    fn is_success(&self) -> bool;

    /// Serialize this output to a JSON value
    #[allow(dead_code)]
    fn to_json(&self) -> Result<serde_json::Value>;
}

/// Automatically implemented for all types that implement both Render, ToolResult and Serialize
impl<T: Render + ToolResult + Serialize + Send + Sync + 'static> AnyOutput for T {
    fn as_render(&self) -> &dyn Render {
        self
    }

    fn is_success(&self) -> bool {
        ToolResult::is_success(self)
    }

    fn to_json(&self) -> Result<serde_json::Value> {
        serde_json::to_value(self).map_err(|e| anyhow::anyhow!("Failed to serialize output: {}", e))
    }
}

/// Type-erased tool interface for storing heterogeneous tools in collections
#[async_trait::async_trait]
pub trait DynTool: Send + Sync + 'static {
    /// Get the static metadata for this tool
    fn spec(&self) -> ToolSpec;

    /// Invoke the tool with JSON parameters and get a type-erased output
    async fn invoke<'a>(
        &self,
        context: &mut ToolContext<'a>,
        params: &mut Value,
    ) -> Result<Box<dyn AnyOutput>>;

    /// Deserialize a JSON value into this tool's output type
    fn deserialize_output(&self, json: Value) -> Result<Box<dyn AnyOutput>>;
}

/// Automatic implementation of DynTool for any type that implements Tool
#[async_trait::async_trait]
impl<T> DynTool for T
where
    T: Tool,
    T::Input: DeserializeOwned,
    T::Output: Render + ToolResult + Serialize + for<'de> Deserialize<'de> + Send + Sync + 'static,
{
    fn spec(&self) -> ToolSpec {
        Tool::spec(self)
    }

    async fn invoke<'a>(
        &self,
        context: &mut ToolContext<'a>,
        params: &mut Value,
    ) -> Result<Box<dyn AnyOutput>> {
        // Deserialize input
        let mut input: T::Input = serde_json::from_value(params.clone()).map_err(|e| {
            // Convert Serde error to ToolError::ParseError
            ToolError::ParseError(format!("Failed to parse parameters: {e}"))
        })?;

        // Execute the tool
        let output = self.execute(context, &mut input).await?;

        // Serialize the potentially updated input back to JSON
        *params = serde_json::to_value(input)
            .map_err(|e| anyhow::anyhow!("Failed to serialize updated input: {}", e))?;

        // Box the output as AnyOutput
        Ok(Box::new(output) as Box<dyn AnyOutput>)
    }

    fn deserialize_output(&self, json: Value) -> Result<Box<dyn AnyOutput>> {
        // Use the tool's deserialize_output method
        let output = Tool::deserialize_output(self, json)?;

        // Box the output as AnyOutput
        Ok(Box::new(output) as Box<dyn AnyOutput>)
    }
}
