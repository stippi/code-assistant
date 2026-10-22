//! MCP client mode wiring: the `mcp-servers.json` configuration file and
//! registration of configured MCP servers' tools into a registry.
//!
//! The protocol client itself lives in the generic `mcp_client` crate; this
//! module binds it to code-assistant's config directory and scope tags.

use crate::tools::core::ToolRegistry;
use crate::tools::scope::capabilities;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use mcp_client::{
    AuthorizationOutcome, AuthorizationRequired, DiscoveredTool, McpServerConfig, McpServerStatus,
    McpServersConfig, McpTransport, OAuthAuthorizer, discover_tools, parse_local_mcp_json,
};

/// Scope tags every MCP tool carries in code-assistant: offered to the main
/// agent and to writing sub-agents (both dialect variants each), not through
/// the MCP server mode. Sub-agents share the parent's registry and session
/// config, so they see the same servers the session has switched on.
pub const MCP_TOOL_SCOPES: &[&str] = &[
    capabilities::SCOPE_AGENT,
    capabilities::SCOPE_AGENT_DIFF,
    capabilities::SCOPE_SUBAGENT_DEFAULT,
    capabilities::SCOPE_SUBAGENT_DEFAULT_DIFF,
];

/// Path of the MCP servers configuration file.
pub fn mcp_servers_config_path() -> PathBuf {
    crate::config_dir::config_dir().join("mcp-servers.json")
}

/// Directory holding persisted OAuth tokens for HTTP MCP servers — one JSON
/// file per server name and URL, written by the interactive login and reused
/// silently on later connects.
pub fn mcp_oauth_dir() -> PathBuf {
    crate::config_dir::config_dir().join("mcp-oauth")
}

/// The server name as a safe file stem, so an unusual name cannot escape
/// [`mcp_oauth_dir`]. Never contains `-`, which separates the URL hash.
fn oauth_file_stem(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// The OAuth token file for server `name`, configured as `server`. A login
/// belongs to the name and the server's URL: a project's server that shares
/// a global server's name neither uses nor replaces the global server's
/// tokens.
pub fn mcp_oauth_token_path(name: &str, server: &McpServerConfig) -> PathBuf {
    let hash = format!("{:x}", md5::compute(oauth_url(server)));
    mcp_oauth_dir().join(format!("{}-{}.json", oauth_file_stem(name), &hash[..16]))
}

/// The URL a server's login belongs to. A stdio server never logs in; it
/// gets a token file that is never written.
fn oauth_url(server: &McpServerConfig) -> &str {
    match &server.transport {
        McpTransport::Http { url, .. } => url,
        _ => "",
    }
}

/// Where tokens were stored by server name only, before logins were kept
/// per URL.
fn legacy_oauth_token_path(name: &str) -> PathBuf {
    mcp_oauth_dir().join(format!("{}.json", oauth_file_stem(name)))
}

/// A login stored by name only came from the global server of that name
/// (project servers could not log in then): move it to that server's token
/// file when `server` is it.
fn adopt_legacy_oauth_tokens(name: &str, server: &McpServerConfig) {
    let legacy = legacy_oauth_token_path(name);
    if !legacy.exists() {
        return;
    }
    let path = mcp_oauth_token_path(name, server);
    let is_global = load_mcp_servers_config()
        .ok()
        .and_then(|config| {
            let global = config.servers.get(name)?;
            Some(global.transport.is_http() && oauth_url(global) == oauth_url(server))
        })
        .unwrap_or(false);
    if is_global
        && !path.exists()
        && let Err(e) = std::fs::rename(&legacy, &path)
    {
        tracing::warn!(
            "Failed to move {} to {}: {e}",
            legacy.display(),
            path.display()
        );
    }
}

/// A fingerprint of the persisted OAuth tokens (file names + sizes + modified
/// times, sorted). Included in the tool-registry fingerprint so that
/// completing an interactive login — which only writes a token file, none of
/// the config files — invalidates the cached registry and the next agent run
/// rebuilds it, reconnecting the now-authorized server so its tools appear
/// without restarting the app.
pub fn mcp_oauth_fingerprint() -> String {
    let dir = mcp_oauth_dir();
    let mut entries: Vec<String> = match std::fs::read_dir(&dir) {
        Ok(read_dir) => read_dir
            .filter_map(|entry| entry.ok())
            .map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let (len, modified) = entry
                    .metadata()
                    .map(|meta| {
                        let modified = meta
                            .modified()
                            .ok()
                            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_nanos())
                            .unwrap_or_default();
                        (meta.len(), modified)
                    })
                    .unwrap_or_default();
                format!("{name}:{len}:{modified}")
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    entries.sort();
    entries.join("\u{0}")
}

/// A persistent credential store for one HTTP MCP server's OAuth tokens,
/// backed by a file under [`mcp_oauth_dir`]. Passed to
/// [`mcp_client::McpServerConnection::connect`] so a stored token is reused
/// without user interaction, and to [`mcp_client::authenticate_http_server`]
/// so the interactive login persists.
pub fn mcp_oauth_credential_store(
    name: &str,
    server: &McpServerConfig,
) -> std::sync::Arc<dyn mcp_client::CredentialStore> {
    adopt_legacy_oauth_tokens(name, server);
    std::sync::Arc::new(mcp_client::FileCredentialStore::new(mcp_oauth_token_path(
        name, server,
    )))
}

/// The configuration of server `name` as an agent run sees it: the global
/// `mcp-servers.json`, overridden by the project-local `.mcp.json` in
/// `local_mcp_dir` when given. Errors if neither defines the server.
pub fn resolve_mcp_server(name: &str, local_mcp_dir: Option<&Path>) -> Result<McpServerConfig> {
    let mut config = load_mcp_servers_config()?;
    if let Some(local) = local_mcp_dir.and_then(load_local_mcp_json) {
        config.merge(local);
    }
    config.servers.remove(name).ok_or_else(|| {
        let mut places = mcp_servers_config_path().display().to_string();
        if let Some(dir) = local_mcp_dir {
            places.push_str(&format!(" or {}", dir.join(".mcp.json").display()));
        }
        anyhow::anyhow!("No MCP server named '{name}' in {places}")
    })
}

/// [`resolve_mcp_server`] for project `project_dir` outside a session (the
/// CLI login), where no one can be asked: the project's `.mcp.json` counts
/// only once it is trusted, as in an agent run.
pub fn resolve_project_mcp_server(name: &str, project_dir: &Path) -> Result<McpServerConfig> {
    if !super::mcp_trust::needs_prompt(project_dir) {
        return resolve_mcp_server(name, Some(project_dir));
    }
    resolve_mcp_server(name, None).with_context(|| {
        format!(
            "{} is not trusted yet; allow its MCP servers in a session of that project first",
            project_dir.join(".mcp.json").display()
        )
    })
}

/// Run the interactive OAuth login for the HTTP MCP server `name`, configured
/// as `server`, and persist its tokens, so later connects reuse them without
/// user interaction. `authorizer` performs the browser round-trip (open the
/// URL, capture the redirect). Errors if `server` is not an HTTP server.
pub async fn authenticate_mcp_server(
    name: &str,
    server: &McpServerConfig,
    authorizer: &dyn OAuthAuthorizer,
) -> Result<()> {
    let store = mcp_oauth_credential_store(name, server);
    mcp_client::authenticate_http_server(name, server, store, authorizer, "code-assistant").await
}

/// Forget every stored OAuth login of server `name`, whatever URL it was
/// made for (e.g. to force a fresh login). Returns how many there were.
pub fn forget_mcp_oauth_tokens(name: &str) -> Result<usize> {
    let stem = oauth_file_stem(name);
    let Ok(entries) = std::fs::read_dir(mcp_oauth_dir()) else {
        return Ok(0);
    };
    let mut removed = 0;
    for entry in entries.filter_map(|entry| entry.ok()) {
        let file_name = entry.file_name();
        let Some(file_stem) = file_name.to_str().and_then(|n| n.strip_suffix(".json")) else {
            continue;
        };
        let of_name = file_stem == stem
            || file_stem
                .strip_prefix(&stem)
                .and_then(|rest| rest.strip_prefix('-'))
                .is_some();
        if of_name {
            let path = entry.path();
            std::fs::remove_file(&path)
                .with_context(|| format!("Failed to remove {}", path.display()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Whether a persisted OAuth login exists for server `name`, configured as
/// `server`. A coarse "have we logged in" signal for status output and UI;
/// it does not check expiry.
pub fn has_mcp_oauth_tokens(name: &str, server: &McpServerConfig) -> bool {
    adopt_legacy_oauth_tokens(name, server);
    mcp_oauth_token_path(name, server).exists()
}

/// Discover a server's tools for a configuration UI, reusing stored OAuth
/// tokens for HTTP servers so an authenticated server lists its tools. An
/// HTTP server that still needs authorization fails with
/// [`AuthorizationRequired`] (downcastable), which the UI turns into an
/// "Authenticate" action.
pub async fn discover_server_tools(
    server_name: &str,
    server: &McpServerConfig,
) -> Result<Vec<DiscoveredTool>> {
    let credentials = server
        .transport
        .is_http()
        .then(|| mcp_oauth_credential_store(server_name, server));
    mcp_client::discover_tools(server_name, server, credentials).await
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
    load_mcp_servers_config_reporting(path).map(|(config, _)| config)
}

/// [`load_mcp_servers_config_from`], also returning the skipped servers.
fn load_mcp_servers_config_reporting(
    path: &Path,
) -> Result<(McpServersConfig, Vec<McpServerStatus>)> {
    if !path.exists() {
        return Ok((McpServersConfig::default(), Vec::new()));
    }
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read MCP config: {}", path.display()))?;
    let mut config: McpServersConfig = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse MCP config: {}", path.display()))?;
    let skipped = substitute_env_values_reporting(&mut config, path);
    Ok((config, skipped))
}

/// Resolve `${ENV_VAR}` patterns in `config` (loaded from `path`), dropping
/// the servers whose variables cannot be resolved. They are logged and
/// returned as failed statuses, so they show up as failing instead of
/// silently missing.
fn substitute_env_values_reporting(
    config: &mut McpServersConfig,
    path: &Path,
) -> Vec<McpServerStatus> {
    config
        .substitute_env_values(|name| std::env::var(name).ok())
        .into_iter()
        .map(|(name, error)| {
            tracing::warn!(
                "Skipping MCP server '{name}' from {}: {error:#}",
                path.display()
            );
            McpServerStatus {
                server: name,
                result: Err(format!("Not started: {error:#}")),
            }
        })
        .collect()
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
    load_local_mcp_json_reporting(dir).map(|(config, _)| config)
}

/// [`load_local_mcp_json`], also returning the skipped servers.
fn load_local_mcp_json_reporting(dir: &Path) -> Option<(McpServersConfig, Vec<McpServerStatus>)> {
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
            let skipped = substitute_env_values_reporting(&mut config, &path);
            Some((config, skipped))
        }
        Err(e) => {
            tracing::warn!("Failed to parse {}: {e:#}", path.display());
            None
        }
    }
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
    let (mut config, mut skipped) =
        match load_mcp_servers_config_reporting(&mcp_servers_config_path()) {
            Ok(loaded) => loaded,
            Err(error) => {
                tracing::warn!("Not registering MCP tools: {error:#}");
                return Vec::new();
            }
        };
    if let Some(dir) = local_mcp_dir
        && let Some((local, local_skipped)) = load_local_mcp_json_reporting(dir)
    {
        let count = local.servers.len();
        config.merge(local);
        skipped.extend(local_skipped);
        tracing::info!(
            "Merged {count} server(s) from .mcp.json in {}",
            dir.display()
        );
    }
    // A skipped server whose name another source still provides (a skipped
    // local override of a global server) is not failing: that one connects.
    skipped.retain(|status| !config.servers.contains_key(&status.server));
    let mut statuses =
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
    statuses.extend(skipped);
    statuses
}

/// The MCP servers whose resources a session reaches (`:mcp:<server>`): the
/// registry's resource servers minus those the session switched off. `None`
/// when none are left.
pub fn session_resource_servers(
    registry: &ToolRegistry,
    disabled_servers: &[String],
) -> Option<Arc<mcp_client::McpResourceServers>> {
    let servers = registry
        .extension::<mcp_client::McpResourceServers>()?
        .without(disabled_servers);
    (!servers.is_empty()).then(|| Arc::new(servers))
}

/// The system prompt section naming the resources of the session's MCP
/// servers (see [`session_resource_servers`]), or `None` without any or
/// where `scope` offers no `read_files` to read them with.
pub fn resources_prompt_section(
    registry: &ToolRegistry,
    scope: crate::tools::core::ToolScope,
    disabled_servers: &[String],
) -> Option<String> {
    if !registry.tool_has_capability("read_files", scope.tag()) {
        return None;
    }
    let servers = session_resource_servers(registry, disabled_servers)?;
    let mut out = String::from("# MCP Resources\n\n");
    out.push_str(
        "These MCP servers expose resources (documents, records, files, …). Read one with \
         `read_files`, passing `:mcp:<server>` as the project and the resource URI as the path \
         (a line range like `<uri>:10-20` works for text). For a template, fill in its \
         {placeholders}. `list_files` with the same project and the path `.` lists a server's \
         current resources; a URI prefix as the path narrows the list.\n",
    );
    for (name, server) in servers.iter() {
        out.push_str(&format!("\n## :mcp:{name}\n\n"));
        let more_hint = format!("list them with `list_files` in `:mcp:{name}`");
        match server.catalog.render_summary(&more_hint) {
            Some(summary) => out.push_str(&summary),
            None => out.push_str(&format!(
                "(no resources listed when the session started: {more_hint})"
            )),
        }
        out.push('\n');
    }
    Some(out)
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
            crate::tools::default_registry_with_mcp(None, pool.as_ref(), false),
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
    fn mcp_tools_reach_the_main_agent_and_writing_sub_agents() {
        use crate::tools::scope::ToolScope;
        for scope in [
            ToolScope::Agent,
            ToolScope::AgentWithDiffBlocks,
            ToolScope::SubAgentDefault,
            ToolScope::SubAgentDefaultWithDiffBlocks,
        ] {
            assert!(MCP_TOOL_SCOPES.contains(&scope.tag()), "{scope:?}");
        }
        for scope in [ToolScope::SubAgentReadOnly, ToolScope::McpServer] {
            assert!(!MCP_TOOL_SCOPES.contains(&scope.tag()), "{scope:?}");
        }
    }

    #[test]
    fn missing_file_yields_empty_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = load_mcp_servers_config_from(&dir.path().join("mcp-servers.json")).unwrap();
        assert!(config.servers.is_empty());
    }

    /// A login finds servers from the project's `.mcp.json` too, which
    /// override global ones of the same name, as in an agent run.
    #[test]
    fn resolve_mcp_server_includes_the_project_config() {
        let config = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            config.path().join("mcp-servers.json"),
            r#"{ "servers": {
                "global": { "url": "https://global.example.com/mcp" },
                "shared": { "url": "https://global.example.com/shared" }
            } }"#,
        )
        .unwrap();
        std::fs::write(
            project.path().join(".mcp.json"),
            r#"{ "mcpServers": {
                "local": { "type": "http", "url": "http://127.0.0.1:9000/mcp" },
                "shared": { "type": "http", "url": "http://127.0.0.1:9000/shared" }
            } }"#,
        )
        .unwrap();
        temp_env::with_var("CODE_ASSISTANT_CONFIG_DIR", Some(config.path()), || {
            let url = |name: &str, dir: Option<&Path>| match resolve_mcp_server(name, dir)
                .unwrap()
                .transport
            {
                McpTransport::Http { url, .. } => url,
                other => panic!("expected an HTTP server, got {other:?}"),
            };
            let dir = Some(project.path());
            assert_eq!(url("global", dir), "https://global.example.com/mcp");
            assert_eq!(url("local", dir), "http://127.0.0.1:9000/mcp");
            assert_eq!(url("shared", dir), "http://127.0.0.1:9000/shared");
            assert_eq!(url("shared", None), "https://global.example.com/shared");

            let error = resolve_mcp_server("local", None).unwrap_err();
            assert!(error.to_string().contains("No MCP server named 'local'"));
        });
    }

    /// Outside a session (the CLI login) a project's `.mcp.json` counts only
    /// once trusted: an untrusted one could point a known server name at
    /// another URL, and the login would store its tokens under that name.
    #[test]
    fn resolve_project_mcp_server_requires_trust() {
        let config = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            config.path().join("mcp-servers.json"),
            r#"{ "servers": { "shared": { "url": "https://global.example.com/shared" } } }"#,
        )
        .unwrap();
        let local = r#"{ "mcpServers": {
            "local": { "type": "http", "url": "http://127.0.0.1:9000/mcp" },
            "shared": { "type": "http", "url": "http://127.0.0.1:9000/shared" }
        } }"#;
        std::fs::write(project.path().join(".mcp.json"), local).unwrap();
        temp_env::with_var("CODE_ASSISTANT_CONFIG_DIR", Some(config.path()), || {
            let url = |name: &str| match resolve_project_mcp_server(name, project.path())
                .unwrap()
                .transport
            {
                McpTransport::Http { url, .. } => url,
                other => panic!("expected an HTTP server, got {other:?}"),
            };
            assert_eq!(url("shared"), "https://global.example.com/shared");
            let error = resolve_project_mcp_server("local", project.path()).unwrap_err();
            assert!(format!("{error:#}").contains("not trusted"), "{error:#}");

            crate::tools::mcp_trust::add_trusted(
                project.path(),
                &crate::tools::mcp_trust::fingerprint(local),
            )
            .unwrap();
            assert_eq!(url("shared"), "http://127.0.0.1:9000/shared");
            assert_eq!(url("local"), "http://127.0.0.1:9000/mcp");
        });
    }

    fn http_server(url: &str) -> McpServerConfig {
        McpServerConfig {
            transport: McpTransport::http(url),
            enabled: true,
            off_in_new_sessions: false,
            enabled_tools: None,
            disabled_tools: Vec::new(),
        }
    }

    /// A login belongs to a server name at one URL: a project server that
    /// shares a global server's name neither sees nor replaces its tokens.
    #[test]
    fn oauth_tokens_are_kept_per_name_and_url() {
        let config = tempfile::tempdir().unwrap();
        temp_env::with_var("CODE_ASSISTANT_CONFIG_DIR", Some(config.path()), || {
            let global = http_server("https://global.example.com/mcp");
            let project = http_server("http://127.0.0.1:9000/mcp");
            let path = mcp_oauth_token_path("remote", &global);
            assert_eq!(path.parent(), Some(mcp_oauth_dir().as_path()));
            assert_ne!(path, mcp_oauth_token_path("remote", &project));
            assert_ne!(path, mcp_oauth_token_path("other", &global));
            assert_eq!(path, mcp_oauth_token_path("remote", &global.clone()));

            std::fs::create_dir_all(mcp_oauth_dir()).unwrap();
            std::fs::write(&path, "{}").unwrap();
            assert!(has_mcp_oauth_tokens("remote", &global));
            assert!(!has_mcp_oauth_tokens("remote", &project));
        });
    }

    /// Tokens stored by name only, before logins were kept per URL, came
    /// from the global server of that name: it takes them over, a project
    /// server of the same name does not.
    #[test]
    fn legacy_oauth_tokens_go_to_the_global_server() {
        let config = tempfile::tempdir().unwrap();
        temp_env::with_var("CODE_ASSISTANT_CONFIG_DIR", Some(config.path()), || {
            std::fs::write(
                config.path().join("mcp-servers.json"),
                r#"{ "servers": { "remote": { "url": "https://global.example.com/mcp" } } }"#,
            )
            .unwrap();
            let legacy = mcp_oauth_dir().join("remote.json");
            std::fs::create_dir_all(mcp_oauth_dir()).unwrap();
            std::fs::write(&legacy, "{}").unwrap();

            let project = http_server("http://127.0.0.1:9000/mcp");
            assert!(!has_mcp_oauth_tokens("remote", &project));
            assert!(legacy.exists());

            let global = http_server("https://global.example.com/mcp");
            assert!(has_mcp_oauth_tokens("remote", &global));
            assert!(!legacy.exists());
            assert!(mcp_oauth_token_path("remote", &global).exists());
        });
    }

    /// Logging out by name forgets every login of that name, whatever URL.
    #[test]
    fn forget_oauth_tokens_removes_every_login_of_the_name() {
        let config = tempfile::tempdir().unwrap();
        temp_env::with_var("CODE_ASSISTANT_CONFIG_DIR", Some(config.path()), || {
            assert_eq!(forget_mcp_oauth_tokens("remote").unwrap(), 0);
            std::fs::create_dir_all(mcp_oauth_dir()).unwrap();
            let paths = [
                mcp_oauth_token_path("remote", &http_server("https://a.example.com/mcp")),
                mcp_oauth_token_path("remote", &http_server("https://b.example.com/mcp")),
                mcp_oauth_dir().join("remote.json"),
            ];
            for path in &paths {
                std::fs::write(path, "{}").unwrap();
            }
            let other = mcp_oauth_token_path("remote_x", &http_server("https://a.example.com/mcp"));
            std::fs::write(&other, "{}").unwrap();

            assert_eq!(forget_mcp_oauth_tokens("remote").unwrap(), 3);
            assert!(paths.iter().all(|path| !path.exists()));
            assert!(other.exists());
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
