//! Which MCP servers a session has switched on, and what the next session of
//! a project starts with.
//!
//! - **Global**: a server of `mcp-servers.json` is on in new sessions unless
//!   it carries `off_in_new_sessions`; the servers of a project's `.mcp.json`
//!   start on.
//! - **Project**: `<config_dir>/project-mcp-servers.json` keeps, per project
//!   root, only the servers whose state differs from their global default.
//!   Switching a server in a session records it for the session's project,
//!   so the project's next session starts the way the last switch left it.
//! - **Session**: `SessionConfig::enabled_mcp_servers` /
//!   `disabled_mcp_servers` hold every server's state from when the session
//!   was created, plus later switches. A server the session has no state for
//!   (added since) is on or off as in a new session of the project.
//!
//! Everything is read raw (no env substitution), so a missing secret does
//! not drop a server from the menu.

use crate::config_dir::config_dir;
use crate::session::SessionConfig;
use crate::tools::mcp::load_mcp_servers_config_raw;
use crate::ui::ui_events::McpServerToggle;
use crate::utils::file_utils::atomic_write_json;
use anyhow::Result;
use mcp_client::parse_local_mcp_json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const PROJECTS_FILE: &str = "project-mcp-servers.json";

/// Name → on, for the entries whose state differs from their default.
type ProjectChoices = BTreeMap<String, bool>;

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProjectStore {
    /// Canonical project root → the project's choices.
    #[serde(default)]
    projects: BTreeMap<String, ProjectChoices>,
}

/// Per-project choices kept in `<config_dir>/<file>`: only the entries whose
/// state differs from their default, per canonical project root. Shared by
/// the MCP server and the built-in tool selection.
pub(crate) struct ProjectChoiceStore {
    file: &'static str,
}

impl ProjectChoiceStore {
    pub(crate) const fn new(file: &'static str) -> Self {
        Self { file }
    }

    fn path(&self) -> PathBuf {
        config_dir().join(self.file)
    }

    fn load(&self) -> ProjectStore {
        let path = self.path();
        let Ok(content) = std::fs::read_to_string(&path) else {
            return ProjectStore::default();
        };
        serde_json::from_str(&content).unwrap_or_else(|e| {
            tracing::warn!("Failed to parse {}: {e}", path.display());
            ProjectStore::default()
        })
    }

    /// The project's choices, empty without a project.
    pub(crate) fn choices(&self, project_root: Option<&Path>) -> ProjectChoices {
        project_root
            .and_then(|root| self.load().projects.remove(&project_key(root)))
            .unwrap_or_default()
    }

    /// Record `on` for `name` in the project, dropping it again when it
    /// matches the default.
    pub(crate) fn remember(
        &self,
        project_root: &Path,
        name: &str,
        on: bool,
        on_by_default: bool,
    ) -> Result<()> {
        let key = project_key(project_root);
        let mut store = self.load();
        let choices = store.projects.entry(key.clone()).or_default();
        if on == on_by_default {
            choices.remove(name);
        } else {
            choices.insert(name.to_string(), on);
        }
        if choices.is_empty() {
            store.projects.remove(&key);
        }
        atomic_write_json(&self.path(), &store)
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.load().projects.is_empty()
    }
}

const PROJECTS: ProjectChoiceStore = ProjectChoiceStore::new(PROJECTS_FILE);

fn project_key(project_root: &Path) -> String {
    // Canonical, so different spellings of a root share one entry.
    std::fs::canonicalize(project_root)
        .unwrap_or_else(|_| project_root.to_path_buf())
        .display()
        .to_string()
}

/// The servers available in `project_dir`, each with whether it is on by
/// default: the enabled global servers, the enabled plugins' servers and the
/// project's `.mcp.json` ones; a later source replaces an earlier server of
/// the same name.
fn available_servers(project_dir: Option<&Path>) -> BTreeMap<String, bool> {
    let mut servers = BTreeMap::new();
    if let Ok(config) = load_mcp_servers_config_raw() {
        for (name, server) in config.enabled_servers() {
            servers.insert(name.clone(), !server.off_in_new_sessions);
        }
    }
    for name in crate::plugin_host::catalog()
        .mcp_servers()
        .config
        .servers
        .into_keys()
    {
        servers.insert(name, true);
    }
    if let Some(dir) = project_dir
        && let Ok(content) = std::fs::read_to_string(dir.join(".mcp.json"))
        && let Ok(local) = parse_local_mcp_json(&content)
    {
        for name in local.servers.into_keys() {
            servers.insert(name, true);
        }
    }
    servers
}

fn session_choice(config: &SessionConfig, name: &str) -> Option<bool> {
    if config.enabled_mcp_servers.iter().any(|n| n == name) {
        Some(true)
    } else if config.disabled_mcp_servers.iter().any(|n| n == name) {
        Some(false)
    } else {
        None
    }
}

/// The MCP servers available to a session and whether each is on for it.
///
/// Listing a project-local server does not depend on trust — the menu shows
/// what the project offers so the user can manage it. Trust separately gates
/// whether those servers actually launch (see [`mcp_trust`]).
///
/// [`mcp_trust`]: crate::tools::mcp_trust
pub fn session_mcp_servers(config: &SessionConfig) -> Vec<McpServerToggle> {
    let project = PROJECTS.choices(config.init_path.as_deref());
    available_servers(config.effective_project_path().map(PathBuf::as_path))
        .into_iter()
        .map(|(name, on_by_default)| {
            let enabled = session_choice(config, &name)
                .or_else(|| project.get(&name).copied())
                .unwrap_or(on_by_default);
            McpServerToggle { name, enabled }
        })
        .collect()
}

/// The servers switched off for a session; their tools stay hidden from its
/// agent.
pub fn session_disabled_mcp_servers(config: &SessionConfig) -> Vec<String> {
    session_mcp_servers(config)
        .into_iter()
        .filter(|server| !server.enabled)
        .map(|server| server.name)
        .collect()
}

/// Record every available server's state in a new session's config: the
/// project's choice, else the global default.
pub fn seed_new_session(config: &mut SessionConfig) {
    config.enabled_mcp_servers.clear();
    config.disabled_mcp_servers.clear();
    for server in session_mcp_servers(config) {
        if server.enabled {
            config.enabled_mcp_servers.push(server.name);
        } else {
            config.disabled_mcp_servers.push(server.name);
        }
    }
}

/// Switch a server on or off in a session's config.
pub fn switch_server(config: &mut SessionConfig, name: &str, on: bool) {
    config.enabled_mcp_servers.retain(|n| n != name);
    config.disabled_mcp_servers.retain(|n| n != name);
    if on {
        config.enabled_mcp_servers.push(name.to_string());
    } else {
        config.disabled_mcp_servers.push(name.to_string());
    }
}

/// Remember a switch in a session of the project the config belongs to, for
/// the project's next sessions. Only a state that differs from the server's
/// default is kept.
pub fn remember_for_project(config: &SessionConfig, name: &str, on: bool) -> Result<()> {
    let Some(root) = config.init_path.as_deref() else {
        return Ok(());
    };
    let on_by_default = available_servers(config.effective_project_path().map(PathBuf::as_path))
        .get(name)
        .copied()
        .unwrap_or(true);
    PROJECTS.remember(root, name, on, on_by_default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_global(config_dir: &Path) {
        std::fs::write(
            config_dir.join("mcp-servers.json"),
            r#"{ "servers": {
                "alpha": { "command": "a" },
                "quiet": { "command": "b", "off_in_new_sessions": true },
                "off": { "command": "c", "enabled": false }
            } }"#,
        )
        .unwrap();
    }

    fn states(servers: Vec<McpServerToggle>) -> Vec<(String, bool)> {
        servers.into_iter().map(|s| (s.name, s.enabled)).collect()
    }

    fn session_in(project: &Path) -> SessionConfig {
        SessionConfig {
            init_path: Some(project.to_path_buf()),
            ..SessionConfig::default()
        }
    }

    #[test]
    fn new_sessions_start_with_the_global_defaults() {
        let config_dir = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_global(config_dir.path());
        temp_env::with_var("CODE_ASSISTANT_CONFIG_DIR", Some(config_dir.path()), || {
            let mut config = session_in(project.path());
            seed_new_session(&mut config);
            assert_eq!(config.enabled_mcp_servers, vec!["alpha".to_string()]);
            assert_eq!(config.disabled_mcp_servers, vec!["quiet".to_string()]);
        });
    }

    #[test]
    fn a_switch_carries_over_to_the_projects_next_session_only() {
        let config_dir = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let other_project = tempfile::tempdir().unwrap();
        write_global(config_dir.path());
        temp_env::with_var("CODE_ASSISTANT_CONFIG_DIR", Some(config_dir.path()), || {
            let mut first = session_in(project.path());
            seed_new_session(&mut first);
            switch_server(&mut first, "alpha", false);
            remember_for_project(&first, "alpha", false).unwrap();
            switch_server(&mut first, "quiet", true);
            remember_for_project(&first, "quiet", true).unwrap();

            let mut next = session_in(project.path());
            seed_new_session(&mut next);
            assert_eq!(
                states(session_mcp_servers(&next)),
                vec![("alpha".to_string(), false), ("quiet".to_string(), true)]
            );

            let mut elsewhere = session_in(other_project.path());
            seed_new_session(&mut elsewhere);
            assert_eq!(
                states(session_mcp_servers(&elsewhere)),
                vec![("alpha".to_string(), true), ("quiet".to_string(), false)]
            );
        });
    }

    #[test]
    fn the_project_keeps_only_differences_from_the_defaults() {
        let config_dir = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_global(config_dir.path());
        temp_env::with_var("CODE_ASSISTANT_CONFIG_DIR", Some(config_dir.path()), || {
            let config = session_in(project.path());
            remember_for_project(&config, "alpha", false).unwrap();
            remember_for_project(&config, "alpha", true).unwrap();
            assert!(PROJECTS.is_empty());
        });
    }

    #[test]
    fn existing_sessions_keep_their_state() {
        let config_dir = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_global(config_dir.path());
        temp_env::with_var("CODE_ASSISTANT_CONFIG_DIR", Some(config_dir.path()), || {
            let mut session = session_in(project.path());
            seed_new_session(&mut session);
            // Another session of the project switches `quiet` on.
            remember_for_project(&session, "quiet", true).unwrap();
            assert_eq!(
                states(session_mcp_servers(&session)),
                vec![("alpha".to_string(), true), ("quiet".to_string(), false)]
            );
        });
    }

    #[test]
    fn a_server_added_later_is_set_as_in_a_new_session() {
        let config_dir = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        temp_env::with_var("CODE_ASSISTANT_CONFIG_DIR", Some(config_dir.path()), || {
            let mut session = session_in(project.path());
            seed_new_session(&mut session);
            write_global(config_dir.path());
            assert_eq!(
                states(session_mcp_servers(&session)),
                vec![("alpha".to_string(), true), ("quiet".to_string(), false)]
            );
            assert_eq!(session_disabled_mcp_servers(&session), vec!["quiet"]);
        });
    }

    #[test]
    fn project_local_servers_are_listed_and_start_on() {
        let config_dir = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join(".mcp.json"),
            r#"{ "mcpServers": { "local": { "type": "stdio", "command": "uv" } } }"#,
        )
        .unwrap();
        temp_env::with_var("CODE_ASSISTANT_CONFIG_DIR", Some(config_dir.path()), || {
            // Listed regardless of trust (trust gates whether it launches).
            let session = session_in(project.path());
            assert_eq!(
                states(session_mcp_servers(&session)),
                vec![("local".to_string(), true)]
            );
            let elsewhere = tempfile::tempdir().unwrap();
            assert!(session_mcp_servers(&session_in(elsewhere.path())).is_empty());
        });
    }
}
