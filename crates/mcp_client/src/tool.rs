//! The registry-facing proxy for one MCP tool: schema from the MCP tool
//! description, execution = MCP `tools/call` round-trip.

use crate::client::McpServerConnection;
use crate::naming::registry_tool_name;
use crate::output::McpToolOutput;
use anyhow::Result;
use rmcp::model::Tool as McpToolDescriptor;
use serde_json::Value;
use std::borrow::Cow;
use std::sync::Arc;
use tools_core::dyn_tool::{AnyOutput, DynTool};
use tools_core::spec::ToolSpec;
use tools_core::tool::ToolContext;

/// A `ToolRegistry` tool backed by an MCP server tool.
pub struct McpTool {
    connection: Arc<McpServerConnection>,
    /// The raw MCP tool name, sent back to the server on `tools/call`.
    remote_name: String,
    /// The (namespaced, sanitized) name the tool is registered under.
    registry_name: String,
    description: String,
    parameters_schema: Value,
    capabilities: Vec<Cow<'static, str>>,
}

impl McpTool {
    pub fn new(
        connection: Arc<McpServerConnection>,
        descriptor: &McpToolDescriptor,
        capabilities: Vec<Cow<'static, str>>,
    ) -> Self {
        let registry_name = registry_tool_name(connection.name(), &descriptor.name);
        Self {
            remote_name: descriptor.name.to_string(),
            registry_name,
            description: descriptor
                .description
                .as_deref()
                .unwrap_or_default()
                .to_string(),
            parameters_schema: Value::Object((*descriptor.input_schema).clone()),
            capabilities,
            connection,
        }
    }

    pub fn registry_name(&self) -> &str {
        &self.registry_name
    }
}

#[async_trait::async_trait]
impl DynTool for McpTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Cow::Owned(self.registry_name.clone()),
            description: Cow::Owned(self.description.clone()),
            parameters_schema: self.parameters_schema.clone(),
            annotations: None,
            capabilities: self.capabilities.clone(),
            multiline_params: &[],
            hidden: false,
            title_template: None,
        }
    }

    async fn invoke<'a>(
        &self,
        _context: &mut ToolContext<'a>,
        params: &mut Value,
    ) -> Result<Box<dyn AnyOutput>> {
        let arguments = match params {
            Value::Null => None,
            Value::Object(map) => Some(map.clone()),
            other => {
                return Err(tools_core::result::ToolError::ParseError(format!(
                    "MCP tool parameters must be a JSON object, got: {other}"
                ))
                .into())
            }
        };
        let output = match self
            .connection
            .call_tool(&self.remote_name, arguments)
            .await
        {
            Ok(result) => McpToolOutput::from_call_result(&result),
            // A dead or failing server degrades to a tool error the agent
            // can react to, never a crashed agent loop.
            Err(error) => McpToolOutput::transport_error(format!("{error:#}")),
        };
        Ok(Box::new(output))
    }

    fn deserialize_output(&self, json: Value) -> Result<Box<dyn AnyOutput>> {
        let output: McpToolOutput = serde_json::from_value(json)?;
        Ok(Box::new(output))
    }
}
