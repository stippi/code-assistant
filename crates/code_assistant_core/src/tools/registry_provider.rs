//! A [`ToolRegistryProvider`] that rebuilds code-assistant's tool registry
//! from the current on-disk configuration (`tools.json` + `mcp-servers.json`)
//! and, for the requested project, its trusted `.mcp.json`.
//!
//! The session manager consults this at the start of every agent run, so
//! configuration edits — e.g. adding an MCP server via the settings page —
//! take effect on the next run without restarting the process. A fingerprint
//! cache keeps rebuilding off the common path where nothing changed: an
//! unchanged request returns the same `Arc`.
//!
//! MCP connections are pooled separately from the registry cache. The pool is
//! keyed by server identity (name + transport), so rebuilding the registry for
//! a different project reuses the still-open global servers instead of
//! relaunching them — only that project's own `.mcp.json` servers connect anew.
//! After each rebuild, pooled connections the new build no longer references
//! (e.g. the previous project's local servers, or a server removed from the
//! config) are evicted and their child processes shut down, so switching
//! projects does not leak processes.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::session::manager::{RegistryRequest, ToolRegistryProvider};
use crate::tools::config::ToolsConfig;
use crate::tools::core::ToolRegistry;
use anyhow::Result;
use async_trait::async_trait;
use mcp_client::{ConnectionProvider, McpServerConfig, McpServerConnection};

struct Cached {
    fingerprint: String,
    registry: Arc<ToolRegistry>,
}

/// Rebuilds the tool registry from disk on demand, caching by a fingerprint of
/// the tool-relevant configuration files and the requested project, and
/// pooling MCP connections across rebuilds.
pub struct ConfigToolRegistry {
    cached: Mutex<Option<Cached>>,
    /// Live MCP connections keyed by [`connection_key`], reused across
    /// rebuilds so shared servers are not relaunched on a project switch.
    connections: Mutex<HashMap<String, Arc<McpServerConnection>>>,
    /// Connection keys requested during the in-progress rebuild, used by
    /// [`Self::evict_unreferenced`] to drop pooled connections the new build no
    /// longer uses. Only meaningful while a rebuild holds the `cached` lock
    /// (rebuilds are serialized by it, and `get_or_connect` runs only within
    /// one), so it is cleared at the start of every rebuild.
    building_keys: Mutex<HashSet<String>>,
}

impl ConfigToolRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cached: Mutex::new(None),
            connections: Mutex::new(HashMap::new()),
            building_keys: Mutex::new(HashSet::new()),
        })
    }

    /// The registry for the global configuration only (no project-local
    /// `.mcp.json`). Used for the process-startup pre-warm before any session
    /// has selected a project.
    pub async fn current(&self) -> Arc<ToolRegistry> {
        self.current_for(RegistryRequest::default()).await
    }

    /// The registry matching `req` — the cached one, or a freshly built one
    /// when `tools.json`, `mcp-servers.json`, the project, or its `.mcp.json`
    /// changed since the last call. Shared MCP servers are reused from the
    /// connection pool.
    pub async fn current_for(&self, req: RegistryRequest) -> Arc<ToolRegistry> {
        let fingerprint = self.fingerprint(&req);
        // The lock is held across the (occasional) rebuild so concurrent
        // callers never build twice; in practice agent runs are serialized by
        // the session manager, so there is no contention on the common path.
        let mut cached = self.cached.lock().await;
        if let Some(entry) = cached.as_ref() {
            if entry.fingerprint == fingerprint {
                return entry.registry.clone();
            }
            tracing::info!("Tool configuration changed; rebuilding the tool registry");
        }
        // Record which connections this build touches so we can shut down the
        // ones it no longer uses. Safe to reset here: the `cached` lock we hold
        // serializes rebuilds, and `get_or_connect` only runs during one.
        self.building_keys.lock().await.clear();
        let registry = crate::tools::default_registry_with_mcp(
            req.project_dir.as_deref(),
            req.include_local_mcp,
            self,
        )
        .await;
        self.evict_unreferenced().await;
        *cached = Some(Cached {
            fingerprint,
            registry: registry.clone(),
        });
        registry
    }

    /// Shut down and drop pooled connections that the just-finished rebuild did
    /// not request (see [`Self::building_keys`]). A connection an in-flight run
    /// still holds (its registry `Arc` outlives this rebuild) is only removed
    /// from the pool here — its last owner terminates the child on drop — so we
    /// never kill a server a running agent is using.
    async fn evict_unreferenced(&self) {
        let live = self.building_keys.lock().await.clone();
        let stale: Vec<(String, Arc<McpServerConnection>)> = {
            let mut connections = self.connections.lock().await;
            let keys: Vec<String> = connections
                .keys()
                .filter(|key| !live.contains(*key))
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|key| connections.remove(&key).map(|conn| (key, conn)))
                .collect()
        };
        for (_key, conn) in stale {
            // Sole owner: terminate the child process explicitly. If still
            // referenced by a live registry, dropping our pool ref is enough —
            // the last owner shuts it down when that run ends.
            if let Ok(conn) = Arc::try_unwrap(conn) {
                tokio::spawn(async move {
                    if let Err(error) = conn.shutdown().await {
                        tracing::warn!("Failed to shut down pooled MCP connection: {error:#}");
                    }
                });
            }
        }
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

    /// Fingerprint of the registry inputs for `req`: the tool-relevant config
    /// files (read raw, so a changed *environment* alone does not trigger a
    /// rebuild — matching process-startup semantics), the requested project,
    /// and, when local MCP is included, the project's `.mcp.json` contents (so
    /// editing it re-prompts and rebuilds).
    fn fingerprint(&self, req: &RegistryRequest) -> String {
        let read = |path: std::path::PathBuf| std::fs::read_to_string(path).unwrap_or_default();
        let tools = ToolsConfig::config_path().map(read).unwrap_or_default();
        let mcp = read(crate::tools::mcp::mcp_servers_config_path());
        let project = req
            .project_dir
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let local = if req.include_local_mcp {
            req.project_dir
                .as_ref()
                .map(|dir| read(dir.join(".mcp.json")))
                .unwrap_or_default()
        } else {
            String::new()
        };
        format!(
            "{tools}\u{0}{mcp}\u{0}{project}\u{0}{}\u{0}{local}",
            req.include_local_mcp
        )
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
        // Mark this server as referenced by the in-progress build so it is not
        // evicted afterwards.
        self.building_keys.lock().await.insert(key.clone());
        if let Some(existing) = self.connections.lock().await.get(&key).cloned() {
            return Ok(existing);
        }
        // Connect outside the lock (slow: process launch / HTTP handshake).
        let connection = Arc::new(McpServerConnection::connect(name, config).await?);
        // Re-check under the lock: if another caller connected the same server
        // meanwhile, keep theirs and drop ours (shut down on drop).
        let stored = self
            .connections
            .lock()
            .await
            .entry(key)
            .or_insert_with(|| connection.clone())
            .clone();
        Ok(stored)
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
}
