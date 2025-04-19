use super::render::Render;
use super::spec::ToolSpec;
use super::tool::{Tool, ToolContext};
use anyhow::Result;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Type-erased tool output that can be rendered
pub trait AnyOutput: Send + Sync {
    /// Get a reference to the output as a Render trait object
    fn as_render(&self) -> &dyn Render;
}

/// Automatically implemented for all types that implement Render
impl<T: Render + Send + Sync + 'static> AnyOutput for T {
    fn as_render(&self) -> &dyn Render {
        self
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
        params: Value
    ) -> Result<Box<dyn AnyOutput>>;
}

/// Automatic implementation of DynTool for any type that implements Tool
#[async_trait::async_trait]
impl<T> DynTool for T
where
    T: Tool,
    T::Input: DeserializeOwned,
    T::Output: Render + Send + Sync + 'static,
{
    fn spec(&self) -> ToolSpec {
        Tool::spec(self)
    }

    async fn invoke<'a>(
        &self,
        context: &mut ToolContext<'a>,
        params: Value
    ) -> Result<Box<dyn AnyOutput>> {
        // Deserialize input
        let input: T::Input = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Failed to parse parameters: {}", e))?;

        // Execute the tool
        let output = self.execute(context, input).await?;

        // Box the output as AnyOutput
        Ok(Box::new(output) as Box<dyn AnyOutput>)
    }
}
