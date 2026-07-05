//! Configuration types for MCP servers.
//!
//! Pure data — file I/O (where the config lives on disk) is the embedder's
//! concern. code-assistant loads this from `mcp-servers.json` in its config
//! directory; other embedders (e.g. pal) construct it programmatically.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// The full MCP client configuration: a named set of servers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct McpServersConfig {
    /// Servers keyed by their (short, human-chosen) name. The name becomes
    /// part of every registered tool's name (`mcp__<server>__<tool>`), so it
    /// should be short and stable.
    #[serde(default)]
    pub servers: BTreeMap<String, McpServerConfig>,
}

impl McpServersConfig {
    /// Servers that are switched on, in stable (sorted) order.
    pub fn enabled_servers(&self) -> impl Iterator<Item = (&String, &McpServerConfig)> {
        self.servers.iter().filter(|(_, server)| server.enabled)
    }
}

/// One configured MCP server, launched as a child process speaking MCP over
/// stdio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Executable to launch.
    pub command: String,
    /// Arguments passed to the executable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Extra environment variables for the child process.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    /// Whether this server is switched on. Disabled servers are not
    /// launched and contribute no tools.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional allowlist: when set, only the named tools are registered.
    /// `None` offers every discovered tool (subject to `disabled_tools`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_tools: Option<Vec<String>>,
    /// Denylist: discovered tools switched off individually (e.g. from the
    /// settings UI). Applied after `enabled_tools`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_tools: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl McpServerConfig {
    /// Whether a discovered tool (by its raw MCP name) should be registered.
    pub fn is_tool_enabled(&self, tool: &str) -> bool {
        let allowed = self
            .enabled_tools
            .as_ref()
            .is_none_or(|allowlist| allowlist.iter().any(|name| name == tool));
        allowed && !self.disabled_tools.iter().any(|name| name == tool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_json_gets_defaults() {
        let config: McpServersConfig =
            serde_json::from_str(r#"{ "servers": { "jira": { "command": "npx" } } }"#).unwrap();
        let jira = &config.servers["jira"];
        assert_eq!(jira.command, "npx");
        assert!(jira.args.is_empty());
        assert!(jira.env.is_empty());
        assert!(jira.enabled);
        assert!(jira.enabled_tools.is_none());
        assert!(jira.disabled_tools.is_empty());
    }

    #[test]
    fn round_trip_omits_default_fields() {
        let config: McpServersConfig =
            serde_json::from_str(r#"{ "servers": { "jira": { "command": "npx" } } }"#).unwrap();
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "servers": { "jira": { "command": "npx", "enabled": true } } })
        );
    }

    #[test]
    fn all_tools_enabled_by_default() {
        let server: McpServerConfig = serde_json::from_str(r#"{ "command": "npx" }"#).unwrap();
        assert!(server.is_tool_enabled("search_issues"));
        assert!(server.is_tool_enabled("create_issue"));
    }

    #[test]
    fn allowlist_restricts_tools() {
        let server: McpServerConfig =
            serde_json::from_str(r#"{ "command": "npx", "enabled_tools": ["search_issues"] }"#)
                .unwrap();
        assert!(server.is_tool_enabled("search_issues"));
        assert!(!server.is_tool_enabled("create_issue"));
    }

    #[test]
    fn denylist_wins_over_allowlist() {
        let server: McpServerConfig = serde_json::from_str(
            r#"{
                "command": "npx",
                "enabled_tools": ["search_issues", "create_issue"],
                "disabled_tools": ["create_issue"]
            }"#,
        )
        .unwrap();
        assert!(server.is_tool_enabled("search_issues"));
        assert!(!server.is_tool_enabled("create_issue"));
    }

    #[test]
    fn enabled_servers_skips_disabled() {
        let config: McpServersConfig = serde_json::from_str(
            r#"{ "servers": {
                "a": { "command": "x", "enabled": false },
                "b": { "command": "y" }
            } }"#,
        )
        .unwrap();
        let names: Vec<_> = config
            .enabled_servers()
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(names, ["b"]);
    }
}
