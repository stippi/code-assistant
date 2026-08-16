//! Connection to a single MCP server, built on the official rmcp SDK.
//!
//! One connection per configured server. For a stdio server the child process
//! lives as long as the connection; for an HTTP server it is a streamable HTTP
//! session. Wrapped tools hold the connection behind an `Arc`, so a dead
//! server degrades to tool errors, never a crashed agent.

use crate::config::{McpServerConfig, McpTransport};
use anyhow::{Context, Result};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, JsonObject, Tool as McpToolDescriptor};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::IntoTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use std::collections::HashMap;
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
    /// Connect to the configured server, running the MCP initialize handshake
    /// over its transport: a launched child process (stdio) or an HTTP
    /// streamable endpoint.
    pub async fn connect(name: &str, config: &McpServerConfig) -> Result<Self> {
        match &config.transport {
            McpTransport::Stdio { command, args, env } => {
                Self::connect_stdio(name, command, args, env).await
            }
            McpTransport::Http { url, headers } => Self::connect_http(name, url, headers).await,
        }
    }

    /// Launch `command` as a child process and run the MCP initialize
    /// handshake over its stdio.
    async fn connect_stdio(
        name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<Self> {
        let mut process = tokio::process::Command::new(command);
        process.args(args).envs(env);
        let transport = rmcp::transport::child_process::TokioChildProcess::new(process)
            .with_context(|| format!("failed to launch MCP server '{name}' ({command})"))?;
        Self::connect_transport(name, transport).await
    }

    /// Connect to an HTTP (streamable) MCP server at `url`, sending the given
    /// custom headers (e.g. `Authorization`) with every request.
    async fn connect_http(
        name: &str,
        url: &str,
        headers: &HashMap<String, String>,
    ) -> Result<Self> {
        let mut config = StreamableHttpClientTransportConfig::with_uri(url.to_string());
        if !headers.is_empty() {
            let mut header_map = HashMap::with_capacity(headers.len());
            for (key, value) in headers {
                let name = http::HeaderName::from_bytes(key.as_bytes())
                    .with_context(|| format!("invalid HTTP header name '{key}'"))?;
                let value = http::HeaderValue::from_str(value)
                    .with_context(|| format!("invalid value for HTTP header '{key}'"))?;
                header_map.insert(name, value);
            }
            config = config.custom_headers(header_map);
        }
        let transport =
            rmcp::transport::streamable_http_client::StreamableHttpClientTransport::from_config(
                config,
            );
        Self::connect_transport(name, transport)
            .await
            .with_context(|| format!("failed to connect to HTTP MCP server '{name}' ({url})"))
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
