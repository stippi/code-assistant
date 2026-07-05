//! Connection to a single MCP server, built on the official rmcp SDK.
//!
//! One connection per configured server; the child process lives as long as
//! the connection. Wrapped tools hold the connection behind an `Arc`, so a
//! dead server degrades to tool errors, never a crashed agent.

use crate::config::McpServerConfig;
use anyhow::{Context, Result};
use rmcp::model::{CallToolRequestParams, CallToolResult, JsonObject, Tool as McpToolDescriptor};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::IntoTransport;
use rmcp::ServiceExt;
use std::time::Duration;

/// Timeout for the initialize handshake and for tool discovery.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout for a single tool call round-trip. Generous: MCP tools may do
/// real work (searches, API calls), but a hung server must not hang a turn
/// forever.
const CALL_TIMEOUT: Duration = Duration::from_secs(300);

/// A live connection to one MCP server.
pub struct McpServerConnection {
    name: String,
    service: RunningService<RoleClient, ()>,
}

impl McpServerConnection {
    /// Launch the configured command as a child process and run the MCP
    /// initialize handshake over its stdio.
    pub async fn connect(name: &str, config: &McpServerConfig) -> Result<Self> {
        let mut command = tokio::process::Command::new(&config.command);
        command.args(&config.args).envs(&config.env);
        let transport = rmcp::transport::child_process::TokioChildProcess::new(command)
            .with_context(|| {
                format!("failed to launch MCP server '{name}' ({})", config.command)
            })?;
        Self::connect_transport(name, transport).await
    }

    /// Run the MCP initialize handshake over an arbitrary transport. Used by
    /// tests (in-process duplex streams); embedders normally use
    /// [`Self::connect`].
    pub async fn connect_transport<T, E, A>(name: &str, transport: T) -> Result<Self>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let service = tokio::time::timeout(CONNECT_TIMEOUT, ().serve(transport))
            .await
            .with_context(|| format!("timeout initializing MCP server '{name}'"))?
            .with_context(|| format!("failed to initialize MCP server '{name}'"))?;
        Ok(Self {
            name: name.to_string(),
            service,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// All tools the server offers (follows pagination).
    pub async fn list_tools(&self) -> Result<Vec<McpToolDescriptor>> {
        tokio::time::timeout(CONNECT_TIMEOUT, self.service.list_all_tools())
            .await
            .with_context(|| format!("timeout listing tools of MCP server '{}'", self.name))?
            .with_context(|| format!("failed to list tools of MCP server '{}'", self.name))
    }

    /// Round-trip a `tools/call` request.
    pub async fn call_tool(
        &self,
        tool: &str,
        arguments: Option<JsonObject>,
    ) -> Result<CallToolResult> {
        let mut params = CallToolRequestParams::new(tool.to_string());
        params.arguments = arguments;
        tokio::time::timeout(CALL_TIMEOUT, self.service.call_tool(params))
            .await
            .with_context(|| {
                format!(
                    "timeout calling tool '{tool}' on MCP server '{}'",
                    self.name
                )
            })?
            .with_context(|| format!("tool '{tool}' failed on MCP server '{}'", self.name))
    }

    /// Close the connection, terminating the server child process. Dropping
    /// the connection has the same effect; this form allows awaiting it.
    pub async fn shutdown(self) -> Result<()> {
        self.service
            .cancel()
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("failed to shut down MCP server '{}': {e}", self.name))
    }
}
