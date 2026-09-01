//! A [`ToolRegistryProvider`] that rebuilds code-assistant's tool registry
//! from the current on-disk configuration (`tools.json` + `mcp-servers.json`)
//! and, for the requested project, its trusted `.mcp.json`.
//!
//! The session manager consults this at the start of every agent run, so
//! configuration edits — e.g. adding an MCP server via the settings page —
//! take effect on the next run without restarting the process. Registries are
//! cached **per project** (keyed by the directory whose `.mcp.json` is
//! included; `None` for the global-only registry), so sessions running in
//! different projects at the same time each keep their own registry: starting
//! a run in one project does not invalidate — or relaunch the servers of —
//! another.
//!
//! Lifetimes are ownership-driven, not managed by explicit eviction. The
//! cache holds only `Weak` references: a registry stays alive exactly as long
//! as something uses it (a session instance between runs, an in-flight run),
//! and each registry holds its MCP connections via `Arc` inside its tools.
//! The connection pool is likewise `Weak`-keyed by server identity (name +
//! transport), so servers shared between builds — typically the global set —
//! are reused while any registry references them, and a server's child
//! process terminates (connection drop) once the last registry using it is
//! gone. No connection a live run holds is ever shut down under it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use tokio::sync::Mutex;

use crate::session::manager::{RegistryRequest, ToolRegistryProvider};
use crate::tools::config::ToolsConfig;
use crate::tools::core::ToolRegistry;
use anyhow::Result;
use async_trait::async_trait;
use mcp_client::{ConnectionProvider, McpServerConfig, McpServerConnection};

/// Cache key: the directory whose `.mcp.json` the registry includes, `None`
/// for the global-only registry. Requests without (trusted) local MCP
/// normalize to `None`, so all such sessions share one registry regardless of
/// their project.
type CacheKey = Option<PathBuf>;

struct Cached {
    fingerprint: String,
    /// Weak: the cache never keeps a registry (or its MCP connections) alive
    /// on its own — sessions and in-flight runs do, via their `Arc`s.
    registry: Weak<ToolRegistry>,
}

/// Rebuilds the tool registry from disk on demand, caching per project by a
/// fingerprint of the tool-relevant configuration files, and pooling MCP
/// connections across builds.
pub struct ConfigToolRegistry {
    cached: Mutex<HashMap<CacheKey, Cached>>,
    /// Live MCP connections keyed by [`connection_key`], reused across
    /// builds so servers shared between registries are not relaunched.
    connections: Mutex<HashMap<String, Weak<McpServerConnection>>>,
}

impl ConfigToolRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cached: Mutex::new(HashMap::new()),
            connections: Mutex::new(HashMap::new()),
        })
    }

    /// The registry for the global configuration only (no project-local
    /// `.mcp.json`). Used for the process-startup pre-warm before any session
    /// has selected a project.
    pub async fn current(&self) -> Arc<ToolRegistry> {
        self.current_for(RegistryRequest::default()).await
    }

    /// The registry matching `req` — the cached one for its project, or a
    /// freshly built one when `tools.json`, `mcp-servers.json`, or the
    /// project's `.mcp.json` changed since that project's last build (or when
    /// nothing holds the cached registry any more). Shared MCP servers are
    /// reused from the connection pool.
    pub async fn current_for(&self, req: RegistryRequest) -> Arc<ToolRegistry> {
        let local_mcp_dir: CacheKey = if req.include_local_mcp {
            req.project_dir
        } else {
            None
        };
        let fingerprint = Self::fingerprint(local_mcp_dir.as_deref());
        // The lock is held across the (occasional) rebuild so concurrent
        // callers never build the same registry twice; in practice agent runs
        // are serialized by the session manager, so there is no contention on
        // the common path.
        let mut cached = self.cached.lock().await;
        if let Some(entry) = cached.get(&local_mcp_dir)
            && entry.fingerprint == fingerprint
            && let Some(registry) = entry.registry.upgrade()
        {
            return registry;
        }
        tracing::info!(
            "Building the tool registry for {}",
            local_mcp_dir
                .as_deref()
                .map(|dir| dir.display().to_string())
                .unwrap_or_else(|| "the global configuration".to_string())
        );
        let registry = crate::tools::default_registry_with_mcp(local_mcp_dir.as_deref(), self).await;
        cached.insert(
            local_mcp_dir,
            Cached {
                fingerprint,
                registry: Arc::downgrade(&registry),
            },
        );
        registry
    }

    /// The provider closure for
    /// [`SessionManager::set_tool_registry_provider`](crate::session::manager::SessionManager::set_tool_registry_provider).
    pub fn as_provider(self: &Arc<Self>) -> ToolRegistryProvider {
        let this = self.clone();
        Arc::new(move |req: RegistryRequest| {
            let this = this.clone();
            Box::pin(async move { this.current_for(req).await })
        })
    }

    /// Fingerprint of the registry inputs: the tool-relevant config files
    /// (read raw, so a changed *environment* alone does not trigger a rebuild
    /// — matching process-startup semantics) and, when a local MCP dir is
    /// included, that directory and its `.mcp.json` contents (so editing it
    /// re-prompts and rebuilds).
    fn fingerprint(local_mcp_dir: Option<&Path>) -> String {
        let read = |path: std::path::PathBuf| std::fs::read_to_string(path).unwrap_or_default();
        let tools = ToolsConfig::config_path().map(read).unwrap_or_default();
        let mcp = read(crate::tools::mcp::mcp_servers_config_path());
        let (dir, local) = local_mcp_dir
            .map(|dir| (dir.display().to_string(), read(dir.join(".mcp.json"))))
            .unwrap_or_default();
        format!("{tools}\u{0}{mcp}\u{0}{dir}\u{0}{local}")
    }
}

/// Identity of an MCP server for the connection pool: its name plus its
/// transport configuration. A changed transport (command, url, headers, …)
/// yields a different key, so the stale connection is not reused.
fn connection_key(name: &str, config: &McpServerConfig) -> String {
    let transport = serde_json::to_string(&config.transport).unwrap_or_default();
    format!("{name}\u{0}{transport}")
}

#[async_trait]
impl ConnectionProvider for ConfigToolRegistry {
    async fn get_or_connect(
        &self,
        name: &str,
        config: &McpServerConfig,
    ) -> Result<Arc<McpServerConnection>> {
        let key = connection_key(name, config);
        {
            let mut connections = self.connections.lock().await;
            // Prune entries whose connection has died with its last registry.
            connections.retain(|_, weak| weak.strong_count() > 0);
            if let Some(existing) = connections.get(&key).and_then(Weak::upgrade) {
                return Ok(existing);
            }
        }
        // Connect outside the lock (slow: process launch / HTTP handshake).
        let connection = Arc::new(McpServerConnection::connect(name, config).await?);
        // Re-check under the lock: if another caller connected the same server
        // meanwhile, keep theirs and drop ours (shut down on drop).
        let mut connections = self.connections.lock().await;
        if let Some(existing) = connections.get(&key).and_then(Weak::upgrade) {
            return Ok(existing);
        }
        connections.insert(key, Arc::downgrade(&connection));
        Ok(connection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcp_client::McpTransport;

    fn cfg(transport: McpTransport) -> McpServerConfig {
        McpServerConfig {
            transport,
            enabled: true,
            enabled_tools: None,
            disabled_tools: Vec::new(),
        }
    }

    #[test]
    fn connection_key_is_stable_and_transport_sensitive() {
        let stdio = cfg(McpTransport::stdio("uv"));
        let stdio_again = cfg(McpTransport::stdio("uv"));
        assert_eq!(
            connection_key("srv", &stdio),
            connection_key("srv", &stdio_again),
            "same name + transport reuses the pooled connection"
        );

        let http = cfg(McpTransport::http("https://host/mcp"));
        assert_ne!(
            connection_key("srv", &stdio),
            connection_key("srv", &http),
            "a changed transport must reconnect, not reuse"
        );
        assert_ne!(
            connection_key("srv", &stdio),
            connection_key("other", &stdio),
            "the server name distinguishes connections"
        );
    }

    fn project_request(dir: &Path) -> RegistryRequest {
        RegistryRequest {
            project_dir: Some(dir.to_path_buf()),
            include_local_mcp: true,
        }
    }

    /// Two projects' registries are cached independently and stay valid at
    /// the same time — a run in one project must not invalidate the other's
    /// registry (the single-slot regression this module used to have).
    #[tokio::test]
    async fn distinct_projects_cache_independently() {
        let config = tempfile::tempdir().unwrap();
        let project_a = tempfile::tempdir().unwrap();
        let project_b = tempfile::tempdir().unwrap();
        std::fs::write(project_a.path().join(".mcp.json"), r#"{"mcpServers":{}}"#).unwrap();
        std::fs::write(project_b.path().join(".mcp.json"), r#"{"mcpServers":{}}"#).unwrap();

        temp_env::async_with_vars(
            [("CODE_ASSISTANT_CONFIG_DIR", Some(config.path()))],
            async {
                let provider = ConfigToolRegistry::new();
                let a = provider.current_for(project_request(project_a.path())).await;
                let b = provider.current_for(project_request(project_b.path())).await;
                assert!(!Arc::ptr_eq(&a, &b), "distinct projects, distinct registries");

                // Alternating requests hit both caches — no thrash.
                let a2 = provider.current_for(project_request(project_a.path())).await;
                let b2 = provider.current_for(project_request(project_b.path())).await;
                assert!(Arc::ptr_eq(&a, &a2), "project A stays cached across B's build");
                assert!(Arc::ptr_eq(&b, &b2), "project B stays cached across A's build");
            },
        )
        .await;
    }

    /// A request without (trusted) local MCP shares the global-only registry,
    /// whatever its project directory is.
    #[tokio::test]
    async fn without_local_mcp_all_projects_share_the_global_registry() {
        let config = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        temp_env::async_with_vars(
            [("CODE_ASSISTANT_CONFIG_DIR", Some(config.path()))],
            async {
                let provider = ConfigToolRegistry::new();
                let global = provider.current().await;
                let projected = provider
                    .current_for(RegistryRequest {
                        project_dir: Some(project.path().to_path_buf()),
                        include_local_mcp: false,
                    })
                    .await;
                assert!(Arc::ptr_eq(&global, &projected));
            },
        )
        .await;
    }

    /// Editing a project's `.mcp.json` rebuilds that project's registry.
    #[tokio::test]
    async fn local_mcp_json_edit_rebuilds() {
        let config = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join(".mcp.json"), r#"{"mcpServers":{}}"#).unwrap();
        temp_env::async_with_vars(
            [("CODE_ASSISTANT_CONFIG_DIR", Some(config.path()))],
            async {
                let provider = ConfigToolRegistry::new();
                let before = provider.current_for(project_request(project.path())).await;
                std::fs::write(project.path().join(".mcp.json"), r#"{ "mcpServers": {} }"#)
                    .unwrap();
                let after = provider.current_for(project_request(project.path())).await;
                assert!(!Arc::ptr_eq(&before, &after), "changed .mcp.json rebuilds");
            },
        )
        .await;
    }

    /// The cache holds only weak references: once nothing uses a registry any
    /// more, it is gone (with its MCP connections) and a later request builds
    /// afresh instead of resurrecting stale state.
    #[tokio::test]
    async fn cache_does_not_keep_registries_alive() {
        let config = tempfile::tempdir().unwrap();
        temp_env::async_with_vars(
            [("CODE_ASSISTANT_CONFIG_DIR", Some(config.path()))],
            async {
                let provider = ConfigToolRegistry::new();
                let registry = provider.current().await;
                let weak = Arc::downgrade(&registry);
                drop(registry);
                assert!(
                    weak.upgrade().is_none(),
                    "the cache must not hold a strong reference"
                );
                // A later request simply rebuilds.
                let _rebuilt = provider.current().await;
            },
        )
        .await;
    }
}
