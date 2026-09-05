//! Configuration types for MCP servers.
//!
//! Pure data — file I/O (where the config lives on disk) is the embedder's
//! concern. code-assistant loads this from `mcp-servers.json` in its config
//! directory; other embedders (e.g. pal) construct it programmatically.

use anyhow::{Context, Result};
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

    /// Substitute `${VAR}` patterns in every server's secret-carrying values
    /// — stdio env values and HTTP header values — so config files can
    /// reference secrets instead of baking them in. `lookup` resolves a
    /// variable name (typically `|name| std::env::var(name).ok()`). A server
    /// with an unresolvable variable (or an unclosed `${`) is **dropped**
    /// rather than failing the whole configuration: missing its secret it
    /// could not connect anyway, but it must not take the other servers down
    /// with it. The dropped servers are returned as `(name, error)` for the
    /// caller to log.
    pub fn substitute_env_values(
        &mut self,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Vec<(String, anyhow::Error)> {
        let mut dropped = Vec::new();
        self.servers
            .retain(|name, server| match server.substitute_env_values(&lookup) {
                Ok(()) => true,
                Err(error) => {
                    dropped.push((name.clone(), error));
                    false
                }
            });
        dropped
    }
}

impl McpServerConfig {
    /// Substitute `${VAR}` in this server's stdio `env` / HTTP `headers`
    /// values (both carry secrets), erroring on the first unresolvable
    /// variable.
    pub fn substitute_env_values(
        &mut self,
        lookup: &impl Fn(&str) -> Option<String>,
    ) -> Result<()> {
        match &mut self.transport {
            McpTransport::Stdio { env, .. } => {
                for value in env.values_mut() {
                    *value = substitute_variables(value, lookup).context("in env")?;
                }
            }
            McpTransport::Http { headers, .. } => {
                for value in headers.values_mut() {
                    *value = substitute_variables(value, lookup).context("in headers")?;
                }
            }
        }
        Ok(())
    }
}

/// Replace every `${VAR}` in `input` with `lookup("VAR")`. Public so
/// embedders can apply the same substitution semantics to other secrets in
/// their own configuration (e.g. API tokens next to the servers section).
pub fn substitute_variables(
    input: &str,
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<String> {
    let mut result = input.to_string();
    while let Some(start) = result.find("${") {
        let end = start
            + result[start..]
                .find('}')
                .ok_or_else(|| anyhow::anyhow!("Unclosed variable substitution: {input}"))?;
        let var_name = &result[start + 2..end];
        let var_value =
            lookup(var_name).with_context(|| format!("Variable not set: {var_name}"))?;
        result.replace_range(start..=end, &var_value);
    }
    Ok(result)
}

/// One configured MCP server. Reached either as a child process speaking MCP
/// over stdio, or over an HTTP (streamable) endpoint — see [`McpTransport`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// How to reach the server. Flattened into the server object so a stdio
    /// server keeps its historical `command`/`args`/`env` shape and an HTTP
    /// server simply carries a `url` (plus optional `headers`); the presence
    /// of `url` selects the HTTP transport.
    #[serde(flatten)]
    pub transport: McpTransport,
    /// Whether this server is switched on. Disabled servers are not
    /// launched/connected and contribute no tools.
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

/// The wire transport used to reach a server. Untagged: the shape of the JSON
/// selects the variant — an object with `url` is HTTP, one with `command` is
/// stdio. This keeps existing (`command`-based) configuration files valid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpTransport {
    /// HTTP streamable transport: the server is reached at `url`.
    Http {
        /// Endpoint URL of the MCP server (e.g. `https://host/mcp`).
        url: String,
        /// Extra HTTP headers sent with every request (e.g. `Authorization`).
        /// Values support `${VAR}` substitution for secrets.
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        headers: HashMap<String, String>,
    },
    /// stdio transport: the configured command is launched as a child
    /// process speaking MCP over its stdio.
    Stdio {
        /// Executable to launch.
        command: String,
        /// Arguments passed to the executable.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        /// Extra environment variables for the child process. Values support
        /// `${VAR}` substitution for secrets.
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        env: HashMap<String, String>,
    },
}

impl McpTransport {
    /// A stdio transport launching `command` with no args and no extra env.
    pub fn stdio(command: impl Into<String>) -> Self {
        McpTransport::Stdio {
            command: command.into(),
            args: Vec::new(),
            env: HashMap::new(),
        }
    }

    /// An HTTP transport pointing at `url` with no extra headers.
    pub fn http(url: impl Into<String>) -> Self {
        McpTransport::Http {
            url: url.into(),
            headers: HashMap::new(),
        }
    }

    /// `true` for the HTTP transport.
    pub fn is_http(&self) -> bool {
        matches!(self, McpTransport::Http { .. })
    }
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

/// Deserialisation types for Claude Code's project-local `.mcp.json` format.
/// The schema differs from our `mcp-servers.json`: the top-level key is
/// `mcpServers` (not `servers`), and each entry carries an explicit `type`
/// field (`"stdio"` / `"http"` / `"sse"`).
mod local_format {
    use super::*;

    #[derive(Deserialize)]
    pub(super) struct LocalMcpFile {
        #[serde(rename = "mcpServers", default)]
        pub mcp_servers: BTreeMap<String, LocalMcpEntry>,
    }

    #[derive(Deserialize)]
    pub(super) struct LocalMcpEntry {
        #[serde(rename = "type", default)]
        pub transport_type: Option<String>,
        // stdio
        pub command: Option<String>,
        #[serde(default)]
        pub args: Vec<String>,
        #[serde(default)]
        pub env: HashMap<String, String>,
        // http / sse
        pub url: Option<String>,
        #[serde(default)]
        pub headers: HashMap<String, String>,
    }

    impl TryFrom<LocalMcpEntry> for McpTransport {
        type Error = anyhow::Error;

        fn try_from(e: LocalMcpEntry) -> Result<Self> {
            let is_http = matches!(
                e.transport_type.as_deref(),
                Some("http") | Some("sse") | Some("streamable-http")
            );
            // An explicit `"type": "stdio"` forces the stdio transport even if a
            // stray `url` is present, so a misconfigured entry surfaces a
            // missing-`command` error rather than being silently reinterpreted
            // as HTTP. A `url` only *implies* HTTP when no type was given.
            let is_stdio = matches!(e.transport_type.as_deref(), Some("stdio"));
            if is_http || (e.url.is_some() && !is_stdio) {
                let url = e
                    .url
                    .ok_or_else(|| anyhow::anyhow!("HTTP server entry is missing 'url'"))?;
                Ok(McpTransport::Http {
                    url,
                    headers: e.headers,
                })
            } else {
                let command = e
                    .command
                    .ok_or_else(|| anyhow::anyhow!("stdio server entry is missing 'command'"))?;
                Ok(McpTransport::Stdio {
                    command,
                    args: e.args,
                    env: e.env,
                })
            }
        }
    }
}

/// Parse the contents of a `.mcp.json` file (Claude Code's project-local MCP
/// config format) into an [`McpServersConfig`]. All servers are enabled with
/// no tool allow- or denylists (the project file carries no such metadata).
pub fn parse_local_mcp_json(content: &str) -> Result<McpServersConfig> {
    let file: local_format::LocalMcpFile =
        serde_json::from_str(content).context("Failed to parse .mcp.json")?;
    let mut servers = BTreeMap::new();
    for (name, entry) in file.mcp_servers {
        let transport = McpTransport::try_from(entry)
            .with_context(|| format!("Invalid entry for server '{name}' in .mcp.json"))?;
        servers.insert(
            name,
            McpServerConfig {
                transport,
                enabled: true,
                enabled_tools: None,
                disabled_tools: Vec::new(),
            },
        );
    }
    Ok(McpServersConfig { servers })
}

impl McpServersConfig {
    /// Merge `other` into `self`, with entries from `other` taking precedence
    /// on name collision. Used to layer a project-local `.mcp.json` on top of
    /// the global `mcp-servers.json`.
    pub fn merge(&mut self, other: McpServersConfig) {
        for (name, server) in other.servers {
            self.servers.insert(name, server);
        }
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
        assert_eq!(jira.transport, McpTransport::stdio("npx"));
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
    fn env_values_get_variables_substituted() {
        let mut config: McpServersConfig = serde_json::from_str(
            r#"{ "servers": { "jira": {
                "command": "npx",
                "env": { "TOKEN": "Bearer ${JIRA_TOKEN}", "PLAIN": "as-is" }
            } } }"#,
        )
        .unwrap();
        let dropped = config
            .substitute_env_values(|name| (name == "JIRA_TOKEN").then(|| "s3cret".to_string()));
        assert!(dropped.is_empty());
        let McpTransport::Stdio { env, .. } = &config.servers["jira"].transport else {
            panic!("expected stdio transport");
        };
        assert_eq!(env["TOKEN"], "Bearer s3cret");
        assert_eq!(env["PLAIN"], "as-is");
    }

    #[test]
    fn http_header_values_get_variables_substituted() {
        let mut config: McpServersConfig = serde_json::from_str(
            r#"{ "servers": { "remote": {
                "url": "https://example.com/mcp",
                "headers": { "Authorization": "Bearer ${API_TOKEN}", "X-Env": "prod" }
            } } }"#,
        )
        .unwrap();
        let dropped = config
            .substitute_env_values(|name| (name == "API_TOKEN").then(|| "s3cret".to_string()));
        assert!(dropped.is_empty());
        let McpTransport::Http { url, headers } = &config.servers["remote"].transport else {
            panic!("expected http transport");
        };
        assert_eq!(url, "https://example.com/mcp");
        assert_eq!(headers["Authorization"], "Bearer s3cret");
        assert_eq!(headers["X-Env"], "prod");
    }

    #[test]
    fn http_server_round_trips() {
        let config: McpServersConfig = serde_json::from_str(
            r#"{ "servers": { "remote": {
                "url": "https://example.com/mcp",
                "headers": { "Authorization": "Bearer x" }
            } } }"#,
        )
        .unwrap();
        assert!(config.servers["remote"].transport.is_http());
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "servers": { "remote": {
                "url": "https://example.com/mcp",
                "headers": { "Authorization": "Bearer x" },
                "enabled": true
            } } })
        );
    }

    #[test]
    fn unresolvable_variable_drops_only_that_server() {
        // A server missing its secret cannot connect anyway; it must not
        // take the other servers down with it.
        let mut config: McpServersConfig = serde_json::from_str(
            r#"{ "servers": {
                "jira": { "command": "npx", "env": { "T": "${MISSING}" } },
                "docs": { "command": "npx", "env": { "T": "${SET}" } }
            } }"#,
        )
        .unwrap();
        let dropped =
            config.substitute_env_values(|name| (name == "SET").then(|| "ok".to_string()));

        assert_eq!(dropped.len(), 1);
        let (name, error) = &dropped[0];
        assert_eq!(name, "jira");
        let error = format!("{error:#}");
        assert!(error.contains("MISSING"), "names the variable: {error}");

        assert!(!config.servers.contains_key("jira"), "failing server gone");
        let McpTransport::Stdio { env, .. } = &config.servers["docs"].transport else {
            panic!("expected stdio transport");
        };
        assert_eq!(env["T"], "ok", "the other server is kept and substituted");
    }

    #[test]
    fn unclosed_substitution_drops_the_server() {
        let mut config: McpServersConfig = serde_json::from_str(
            r#"{ "servers": { "jira": { "command": "npx", "env": { "T": "${OOPS" } } } }"#,
        )
        .unwrap();
        let dropped = config.substitute_env_values(|_| Some("x".to_string()));
        assert_eq!(dropped.len(), 1);
        assert!(config.servers.is_empty());
    }

    #[test]
    fn commands_and_args_are_left_alone() {
        // Substitution is deliberately limited to env/header values — commands
        // and args come from the same trusted file, but only env carries
        // secrets.
        let mut config: McpServersConfig = serde_json::from_str(
            r#"{ "servers": { "jira": { "command": "${CMD}", "args": ["${ARG}"] } } }"#,
        )
        .unwrap();
        assert!(config.substitute_env_values(|_| None).is_empty());
        assert_eq!(
            config.servers["jira"].transport,
            McpTransport::Stdio {
                command: "${CMD}".to_string(),
                args: vec!["${ARG}".to_string()],
                env: HashMap::new(),
            }
        );
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

    #[test]
    fn parse_local_mcp_json_stdio() {
        let json = r#"{
            "mcpServers": {
                "backlog": {
                    "type": "stdio",
                    "command": "uv",
                    "args": ["run", "server.py"],
                    "env": { "TOKEN": "abc" }
                }
            }
        }"#;
        let config = parse_local_mcp_json(json).unwrap();
        assert_eq!(config.servers.len(), 1);
        let server = &config.servers["backlog"];
        assert!(server.enabled);
        assert!(server.enabled_tools.is_none());
        assert!(server.disabled_tools.is_empty());
        assert_eq!(
            server.transport,
            McpTransport::Stdio {
                command: "uv".to_string(),
                args: vec!["run".to_string(), "server.py".to_string()],
                env: [("TOKEN".to_string(), "abc".to_string())].into(),
            }
        );
    }

    #[test]
    fn parse_local_mcp_json_http() {
        let json = r#"{
            "mcpServers": {
                "jira": { "type": "http", "url": "https://example.com/mcp" }
            }
        }"#;
        let config = parse_local_mcp_json(json).unwrap();
        let server = &config.servers["jira"];
        assert_eq!(
            server.transport,
            McpTransport::Http {
                url: "https://example.com/mcp".to_string(),
                headers: HashMap::new(),
            }
        );
    }

    #[test]
    fn parse_local_mcp_json_sse_transport_name() {
        let json = r#"{
            "mcpServers": {
                "old": { "type": "sse", "url": "https://example.com/sse" }
            }
        }"#;
        let config = parse_local_mcp_json(json).unwrap();
        assert!(config.servers["old"].transport.is_http());
    }

    #[test]
    fn parse_local_mcp_json_implicit_http_via_url() {
        let json = r#"{
            "mcpServers": {
                "srv": { "url": "https://example.com/mcp" }
            }
        }"#;
        let config = parse_local_mcp_json(json).unwrap();
        assert!(config.servers["srv"].transport.is_http());
    }

    #[test]
    fn parse_local_mcp_json_empty_mcp_servers() {
        let config = parse_local_mcp_json(r#"{ "mcpServers": {} }"#).unwrap();
        assert!(config.servers.is_empty());
    }

    #[test]
    fn merge_local_overrides_global() {
        let mut global: McpServersConfig = serde_json::from_str(
            r#"{
            "servers": {
                "a": { "command": "global-a" },
                "b": { "command": "global-b" }
            }
        }"#,
        )
        .unwrap();
        let local: McpServersConfig = serde_json::from_str(
            r#"{
            "servers": { "b": { "command": "local-b" }, "c": { "command": "local-c" } }
        }"#,
        )
        .unwrap();
        global.merge(local);
        assert_eq!(global.servers.len(), 3);
        let McpTransport::Stdio { command, .. } = &global.servers["b"].transport else {
            panic!("expected stdio");
        };
        assert_eq!(command, "local-b", "local should win on collision");
        assert!(
            global.servers.contains_key("a"),
            "global-only entry preserved"
        );
        assert!(global.servers.contains_key("c"), "local-only entry added");
    }
}
