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
    DiscoveredTool, McpServerConfig, McpServerStatus, McpServersConfig, McpTransport,
    discover_tools, parse_local_mcp_json,
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
/// server environment values. A server whose variables cannot be resolved is
/// skipped with a log warning (it could not connect anyway); a missing file
/// yields the default (empty) configuration.
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
    for (name, error) in config.substitute_env_values(|name| std::env::var(name).ok()) {
        tracing::warn!(
            "Skipping MCP server '{name}' from {}: {error:#}",
            path.display()
        );
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

/// Whether [`deserialize_tool_execution`] can resolve this recorded
/// execution. MCP executions and parse errors always can; a native tool must
/// still be present in `registry`.
pub fn execution_renderable(
    se: &agent_core::SerializedToolExecution,
    registry: &ToolRegistry,
) -> bool {
    mcp_client::is_mcp_tool_name(&se.tool_name) || se.tool_available(registry)
}

/// Deserialize a recorded tool execution for rendering or session state. MCP
/// executions (`mcp__…` names) deserialize from their self-describing JSON,
/// deliberately independent of which servers `registry` has connected: a
/// session must render identically wherever it is viewed — under another
/// project's registry, or in an instance that never launched (or never
/// trusted) the producing server.
pub fn deserialize_tool_execution(
    se: &agent_core::SerializedToolExecution,
    registry: &ToolRegistry,
) -> Result<agent_core::ToolExecution> {
    if mcp_client::is_mcp_tool_name(&se.tool_name) {
        return Ok(agent_core::ToolExecution {
            tool_request: se.tool_request.clone(),
            result: mcp_client::deserialize_mcp_output(se.result_json.clone())?,
        });
    }
    se.deserialize(registry)
}

/// Load a project-local `.mcp.json` from `dir`, if present. Returns `None`
/// when the file does not exist; logs a warning on parse errors rather than
/// propagating them, so a malformed local file degrades gracefully. A server
/// whose `${VAR}` references cannot be resolved is skipped with a log
/// warning — the other servers still load.
pub fn load_local_mcp_json(dir: &Path) -> Option<McpServersConfig> {
    let path = dir.join(".mcp.json");
    if !path.exists() {
        return None;
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to read {}: {e}", path.display());
            return None;
        }
    };
    match parse_local_mcp_json(&content) {
        Ok(mut config) => {
            for (name, error) in config.substitute_env_values(|name| std::env::var(name).ok()) {
                tracing::warn!(
                    "Skipping MCP server '{name}' from {}: {error:#}",
                    path.display()
                );
            }
            Some(config)
        }
        Err(e) => {
            tracing::warn!("Failed to parse {}: {e:#}", path.display());
            None
        }
    }
}

/// The MCP servers available to a session and whether each is enabled for it.
/// The list is the global `mcp-servers.json` enabled servers plus, when the
/// project declares a `.mcp.json`, that file's servers; a name present in
/// `disabled` is reported as disabled. Read raw (no env substitution) so a
/// missing secret does not drop a server from the menu.
///
/// Listing a project-local server does not depend on trust — the menu shows
/// what the project offers so the user can manage it. Trust separately gates
/// whether those servers actually launch (see [`mcp_trust`]).
///
/// [`mcp_trust`]: crate::tools::mcp_trust
pub fn session_mcp_servers(
    project_dir: Option<&Path>,
    disabled: &[String],
) -> Vec<crate::ui::ui_events::McpServerToggle> {
    use crate::ui::ui_events::McpServerToggle;

    let mut names: Vec<String> = Vec::new();
    if let Ok(config) = load_mcp_servers_config_raw() {
        for (name, _) in config.enabled_servers() {
            names.push(name.clone());
        }
    }
    if let Some(dir) = project_dir
        && let Ok(content) = std::fs::read_to_string(dir.join(".mcp.json"))
        && let Ok(local) = parse_local_mcp_json(&content)
    {
        for name in local.servers.keys() {
            names.push(name.clone());
        }
    }
    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(|name| {
            let enabled = !disabled.iter().any(|d| d == &name);
            McpServerToggle { name, enabled }
        })
        .collect()
}

/// Connect to all enabled servers from `mcp-servers.json` (plus, when
/// `local_mcp_dir` is set, the project-local `.mcp.json` in that directory)
/// and register their enabled tools with code-assistant's scope tags.
/// Connections are obtained from `pool`, so servers shared with a previous
/// build (e.g. the global set, unchanged when only a project differs) stay
/// connected instead of being relaunched. Failures degrade to log warnings —
/// a broken MCP setup must not prevent startup.
pub async fn register_configured_mcp_tools_in(
    registry: &mut ToolRegistry,
    local_mcp_dir: Option<&Path>,
    pool: &dyn mcp_client::ConnectionProvider,
) -> Vec<McpServerStatus> {
    let mut config = match load_mcp_servers_config() {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!("Not registering MCP tools: {error:#}");
            return Vec::new();
        }
    };
    if let Some(dir) = local_mcp_dir
        && let Some(local) = load_local_mcp_json(dir)
    {
        let count = local.servers.len();
        config.merge(local);
        tracing::info!(
            "Merged {count} server(s) from .mcp.json in {}",
            dir.display()
        );
    }
    let statuses =
        mcp_client::register_mcp_tools_pooled(registry, &config, MCP_TOOL_SCOPES, pool).await;
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

    /// Full core path: mcp-servers.json in the config dir → registry with
    /// namespaced, scope-tagged MCP tools, served by a real child process
    /// (code-assistant's own MCP server mode). Ignored by default: needs the
    /// workspace binary built. The CODE_ASSISTANT_CONFIG_DIR override is set
    /// only for the duration of the awaited registry build via `temp_env`.
    #[tokio::test]
    #[ignore = "needs a built code-assistant binary in target/debug"]
    async fn default_registry_with_mcp_offers_server_tools() {
        let binary = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/debug/code-assistant");
        assert!(binary.exists(), "build the code-assistant binary first");

        let dir = tempfile::tempdir().unwrap();
        save_mcp_servers_config_to(
            &dir.path().join("mcp-servers.json"),
            &serde_json::from_value(serde_json::json!({
                "servers": { "self": {
                    "command": binary.to_string_lossy(),
                    "args": ["server"],
                    "enabled_tools": ["read_files"]
                } }
            }))
            .unwrap(),
        )
        .unwrap();

        let pool = crate::tools::ConfigToolRegistry::new();
        let registry = temp_env::async_with_vars(
            [("CODE_ASSISTANT_CONFIG_DIR", Some(dir.path()))],
            crate::tools::default_registry_with_mcp(None, pool.as_ref()),
        )
        .await;

        let definitions = registry
            .get_tool_definitions_with_capability(crate::tools::scope::ToolScope::Agent.tag());
        assert!(
            definitions
                .iter()
                .any(|tool| tool.name == "mcp__self__read_files"),
            "agent scope must offer the MCP tool"
        );
        // The allowlist keeps every other server tool out.
        assert!(!definitions.iter().any(
            |tool| tool.name.starts_with("mcp__self__") && tool.name != "mcp__self__read_files"
        ));
    }

    #[test]
    fn missing_file_yields_empty_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = load_mcp_servers_config_from(&dir.path().join("mcp-servers.json")).unwrap();
        assert!(config.servers.is_empty());
    }

    #[test]
    fn session_mcp_servers_lists_enabled_global_and_marks_disabled() {
        let config_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            config_dir.path().join("mcp-servers.json"),
            r#"{ "servers": {
                "alpha": { "command": "a" },
                "beta": { "command": "b" },
                "off": { "command": "c", "enabled": false }
            } }"#,
        )
        .unwrap();
        temp_env::with_var("CODE_ASSISTANT_CONFIG_DIR", Some(config_dir.path()), || {
            let servers = session_mcp_servers(None, &["beta".to_string()]);
            let by_name: std::collections::HashMap<_, _> = servers
                .iter()
                .map(|s| (s.name.as_str(), s.enabled))
                .collect();
            assert_eq!(by_name.get("alpha"), Some(&true));
            assert_eq!(
                by_name.get("beta"),
                Some(&false),
                "session-disabled server is marked off"
            );
            assert!(
                !by_name.contains_key("off"),
                "globally-disabled server is not listed"
            );
        });
    }

    #[test]
    fn session_mcp_servers_includes_project_local_declared() {
        let config_dir = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join(".mcp.json"),
            r#"{ "mcpServers": { "local": { "type": "stdio", "command": "uv" } } }"#,
        )
        .unwrap();
        temp_env::with_var("CODE_ASSISTANT_CONFIG_DIR", Some(config_dir.path()), || {
            // A declared project-local server is listed regardless of trust
            // (trust separately gates whether it launches).
            let servers = session_mcp_servers(Some(project.path()), &[]);
            assert!(
                servers.iter().any(|s| s.name == "local" && s.enabled),
                "project-declared server is listed and enabled"
            );
            // And a project without a .mcp.json contributes nothing local.
            let empty = tempfile::tempdir().unwrap();
            assert!(
                !session_mcp_servers(Some(empty.path()), &[])
                    .iter()
                    .any(|s| s.name == "local")
            );
        });
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
        temp_env::with_var("MCP_TEST_TOKEN", Some("secret-123"), || {
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
            let McpTransport::Stdio { env, .. } = &loaded.servers["jira"].transport else {
                panic!("expected stdio transport");
            };
            assert_eq!(env["API_TOKEN"], "secret-123");
        });
    }

    #[test]
    fn unknown_env_var_drops_only_that_server() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-servers.json");
        std::fs::write(
            &path,
            r#"{ "servers": {
                "jira": {
                    "command": "npx",
                    "env": { "API_TOKEN": "${MCP_TEST_SURELY_UNSET}" }
                },
                "docs": { "command": "npx" }
            } }"#,
        )
        .unwrap();
        let config = load_mcp_servers_config_from(&path).unwrap();
        assert!(
            !config.servers.contains_key("jira"),
            "server with unresolvable variable is dropped"
        );
        assert!(
            config.servers.contains_key("docs"),
            "the other server survives"
        );
    }

    #[test]
    fn mcp_execution_renders_without_the_server_registered() {
        // A session may be viewed by an instance that never connected (or
        // never trusted) the producing server — the recorded execution must
        // still deserialize, from its self-describing JSON alone.
        let se = agent_core::SerializedToolExecution {
            tool_request: agent_core::ToolRequest {
                id: "1".into(),
                name: "mcp__jira__search".into(),
                input: serde_json::Value::Null,
                start_offset: None,
                end_offset: None,
            },
            result_json: serde_json::json!({ "text": "3 issues", "is_error": false }),
            tool_name: "mcp__jira__search".into(),
        };
        let empty = ToolRegistry::new();
        assert!(execution_renderable(&se, &empty));
        let execution = deserialize_tool_execution(&se, &empty).unwrap();
        assert!(execution.result.is_success());
        assert_eq!(execution.result.as_render().status(), "3 issues");
    }

    #[test]
    fn native_execution_still_requires_its_tool() {
        let se = agent_core::SerializedToolExecution {
            tool_request: agent_core::ToolRequest {
                id: "1".into(),
                name: "vanished_tool".into(),
                input: serde_json::Value::Null,
                start_offset: None,
                end_offset: None,
            },
            result_json: serde_json::json!({}),
            tool_name: "vanished_tool".into(),
        };
        let empty = ToolRegistry::new();
        assert!(!execution_renderable(&se, &empty));
        assert!(deserialize_tool_execution(&se, &empty).is_err());
    }

    #[test]
    fn load_local_mcp_json_absent_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_local_mcp_json(dir.path()).is_none());
    }

    #[test]
    fn load_local_mcp_json_parses_and_substitutes() {
        temp_env::with_var("LOCAL_MCP_SECRET", Some("tok-xyz"), || {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join(".mcp.json"),
                r#"{ "mcpServers": { "srv": {
                    "type": "stdio",
                    "command": "uv",
                    "env": { "TOKEN": "${LOCAL_MCP_SECRET}" }
                } } }"#,
            )
            .unwrap();
            let config = load_local_mcp_json(dir.path()).unwrap();
            let McpTransport::Stdio { env, .. } = &config.servers["srv"].transport else {
                panic!("expected stdio");
            };
            assert_eq!(env["TOKEN"], "tok-xyz");
        });
    }

    #[test]
    fn load_local_mcp_json_drops_only_the_failing_server() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".mcp.json"),
            r#"{ "mcpServers": {
                "broken": {
                    "type": "stdio",
                    "command": "uv",
                    "env": { "TOKEN": "${MCP_TEST_SURELY_UNSET}" }
                },
                "fine": { "type": "stdio", "command": "uv" }
            } }"#,
        )
        .unwrap();
        let config = load_local_mcp_json(dir.path()).unwrap();
        assert!(
            !config.servers.contains_key("broken"),
            "server with unresolvable variable is dropped"
        );
        assert!(
            config.servers.contains_key("fine"),
            "the other server survives"
        );
    }

    #[test]
    fn load_local_mcp_json_malformed_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".mcp.json"), b"not json").unwrap();
        assert!(load_local_mcp_json(dir.path()).is_none());
    }
}
