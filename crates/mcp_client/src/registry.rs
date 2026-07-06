//! Registering MCP tools in a `ToolRegistry`.
//!
//! `register_mcp_tools` is the whole client mode from the embedder's point of
//! view: connect to every enabled server, list its tools, wrap and register
//! the enabled ones. A server that fails to start contributes an error status
//! instead of tools.

use crate::client::McpServerConnection;
use crate::config::{McpServerConfig, McpServersConfig};
use crate::tool::McpTool;
use anyhow::Result;
use std::borrow::Cow;
use std::sync::Arc;
use tools_core::registry::ToolRegistry;

/// Capability tag carried by every MCP-backed tool.
pub const MCP_CAPABILITY: &str = "mcp";

/// The per-server scope tag (`scope:mcp-<server>`), usable for additive
/// scope selection by embedders.
pub fn server_scope_capability(server: &str) -> String {
    format!("scope:mcp-{}", crate::naming::sanitize(server))
}

/// Outcome of connecting one configured server: the registry names of the
/// tools it contributed, or the error that prevented it.
#[derive(Debug)]
pub struct McpServerStatus {
    pub server: String,
    pub result: Result<Vec<String>, String>,
}

/// Connect to all enabled servers in `config` and register their enabled
/// tools. Every registered tool carries [`MCP_CAPABILITY`], its server's
/// scope tag, and the given extra capability tags (the embedder's scope
/// vocabulary, e.g. `scope:agent`).
pub async fn register_mcp_tools(
    registry: &mut ToolRegistry,
    config: &McpServersConfig,
    extra_capabilities: &[&'static str],
) -> Vec<McpServerStatus> {
    let mut statuses = Vec::new();
    for (name, server_config) in config.enabled_servers() {
        let result = async {
            let connection = McpServerConnection::connect(name, server_config).await?;
            register_connection_tools(
                registry,
                Arc::new(connection),
                server_config,
                extra_capabilities,
            )
            .await
        }
        .await;
        statuses.push(McpServerStatus {
            server: name.clone(),
            result: result.map_err(|error| format!("{error:#}")),
        });
    }
    statuses
}

/// Register the enabled tools of an already-connected server. Split out so
/// tests (and embedders with custom transports) can pass their own
/// connection.
pub async fn register_connection_tools(
    registry: &mut ToolRegistry,
    connection: Arc<McpServerConnection>,
    config: &McpServerConfig,
    extra_capabilities: &[&'static str],
) -> Result<Vec<String>> {
    let descriptors = connection.list_tools().await?;
    let mut registered = Vec::new();
    for descriptor in descriptors {
        if !config.is_tool_enabled(&descriptor.name) {
            continue;
        }
        let capabilities = capabilities_for(connection.name(), extra_capabilities);
        let tool = McpTool::new(connection.clone(), &descriptor, capabilities);
        registered.push(tool.registry_name().to_string());
        registry.register(Box::new(tool));
    }
    tracing::info!(
        server = connection.name(),
        tools = registered.len(),
        "registered MCP tools"
    );
    Ok(registered)
}

fn capabilities_for(server: &str, extra: &[&'static str]) -> Vec<Cow<'static, str>> {
    let mut capabilities: Vec<Cow<'static, str>> = Vec::with_capacity(extra.len() + 2);
    capabilities.push(Cow::Borrowed(MCP_CAPABILITY));
    capabilities.push(Cow::Owned(server_scope_capability(server)));
    capabilities.extend(extra.iter().map(|tag| Cow::Borrowed(*tag)));
    capabilities
}

/// A tool discovered on a server, reduced to what a configuration UI needs.
#[derive(Debug, Clone)]
pub struct DiscoveredTool {
    pub name: String,
    pub description: String,
}

/// Connect to a server, list everything it offers (ignoring the tool
/// filter), and shut the connection down again. For configuration UIs.
pub async fn discover_tools(
    server_name: &str,
    config: &McpServerConfig,
) -> Result<Vec<DiscoveredTool>> {
    let connection = McpServerConnection::connect(server_name, config).await?;
    let descriptors = connection.list_tools().await?;
    let _ = connection.shutdown().await;
    Ok(descriptors
        .into_iter()
        .map(|descriptor| DiscoveredTool {
            name: descriptor.name.to_string(),
            description: descriptor
                .description
                .as_deref()
                .unwrap_or_default()
                .to_string(),
        })
        .collect())
}
