//! MCP client mode wiring: the `mcp-servers.json` configuration file and
//! registration of configured MCP servers' tools into a registry.
//!
//! The protocol client itself lives in the generic `mcp_client` crate; this
//! module binds it to code-assistant's config directory and scope tags.

use crate::tools::core::ToolRegistry;
use crate::tools::scope::capabilities;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub use mcp_client::{
    discover_tools, DiscoveredTool, McpServerConfig, McpServerStatus, McpServersConfig,
};

/// Scope tags every MCP tool carries in code-assistant: offered to the main
/// agent (both dialect variants), not to sub-agents and not through the MCP
/// server mode.
pub const MCP_TOOL_SCOPES: &[&str] = &[capabilities::SCOPE_AGENT, capabilities::SCOPE_AGENT_DIFF];

/// Path of the MCP servers configuration file.
pub fn mcp_servers_config_path() -> PathBuf {
    crate::config_dir::config_dir().join("mcp-servers.json")
}

/// Load the MCP servers configuration, substituting `${ENV_VAR}` patterns in
/// server environment values. A missing file yields the default (empty)
/// configuration.
pub fn load_mcp_servers_config() -> Result<McpServersConfig> {
    load_mcp_servers_config_from(&mcp_servers_config_path())
}

/// [`load_mcp_servers_config`] from an explicit path (testable).
pub fn load_mcp_servers_config_from(path: &Path) -> Result<McpServersConfig> {
    if !path.exists() {
        return Ok(McpServersConfig::default());
    }
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read MCP config: {}", path.display()))?;
    let mut config: McpServersConfig = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse MCP config: {}", path.display()))?;
    for (name, server) in config.servers.iter_mut() {
        for value in server.env.values_mut() {
            *value = crate::tools::core::ToolsConfig::substitute_env_var_in_string(value)
                .with_context(|| format!("in env of MCP server '{name}'"))?;
        }
    }
    Ok(config)
}

/// Load the MCP servers configuration verbatim, without `${ENV_VAR}`
/// substitution — for editing UIs, which must show and preserve the raw
/// placeholders instead of baked-in secrets.
pub fn load_mcp_servers_config_raw() -> Result<McpServersConfig> {
    let path = mcp_servers_config_path();
    if !path.exists() {
        return Ok(McpServersConfig::default());
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read MCP config: {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse MCP config: {}", path.display()))
}

/// Persist the MCP servers configuration (raw, without env substitution).
pub fn save_mcp_servers_config(config: &McpServersConfig) -> Result<()> {
    save_mcp_servers_config_to(&mcp_servers_config_path(), config)
}

/// [`save_mcp_servers_config`] to an explicit path (testable).
pub fn save_mcp_servers_config_to(path: &Path, config: &McpServersConfig) -> Result<()> {
    crate::utils::file_utils::atomic_write_json(path, config)
        .with_context(|| format!("Failed to write MCP config: {}", path.display()))
}

/// Connect to all enabled servers from `mcp-servers.json` and register their
/// enabled tools with code-assistant's scope tags. Failures degrade to log
/// warnings — a broken MCP setup must not prevent startup.
pub async fn register_configured_mcp_tools(registry: &mut ToolRegistry) -> Vec<McpServerStatus> {
    let config = match load_mcp_servers_config() {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!("Not registering MCP tools: {error:#}");
            return Vec::new();
        }
    };
    let statuses = mcp_client::register_mcp_tools(registry, &config, MCP_TOOL_SCOPES).await;
    for status in &statuses {
        match &status.result {
            Ok(tools) => tracing::info!(
                server = status.server,
                "MCP server contributed {} tool(s)",
                tools.len()
            ),
            Err(error) => {
                tracing::warn!(server = status.server, "MCP server failed: {error}")
            }
        }
    }
    statuses
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_empty_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = load_mcp_servers_config_from(&dir.path().join("mcp-servers.json")).unwrap();
        assert!(config.servers.is_empty());
    }

    #[test]
    fn save_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-servers.json");
        let config: McpServersConfig = serde_json::from_value(serde_json::json!({
            "servers": {
                "jira": {
                    "command": "npx",
                    "args": ["-y", "some-jira-server"],
                    "disabled_tools": ["delete_project"]
                }
            }
        }))
        .unwrap();
        save_mcp_servers_config_to(&path, &config).unwrap();
        let loaded = load_mcp_servers_config_from(&path).unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn env_values_are_substituted_at_load() {
        std::env::set_var("MCP_TEST_TOKEN", "secret-123");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-servers.json");
        std::fs::write(
            &path,
            r#"{ "servers": { "jira": {
                "command": "npx",
                "env": { "API_TOKEN": "${MCP_TEST_TOKEN}" }
            } } }"#,
        )
        .unwrap();
        let loaded = load_mcp_servers_config_from(&path).unwrap();
        assert_eq!(loaded.servers["jira"].env["API_TOKEN"], "secret-123");
        std::env::remove_var("MCP_TEST_TOKEN");
    }

    #[test]
    fn unknown_env_var_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-servers.json");
        std::fs::write(
            &path,
            r#"{ "servers": { "jira": {
                "command": "npx",
                "env": { "API_TOKEN": "${MCP_TEST_SURELY_UNSET}" }
            } } }"#,
        )
        .unwrap();
        assert!(load_mcp_servers_config_from(&path).is_err());
    }
}
