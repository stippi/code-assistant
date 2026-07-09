//! A [`ToolRegistryProvider`] that rebuilds code-assistant's tool registry
//! from the current on-disk configuration (`tools.json` + `mcp-servers.json`).
//!
//! The session manager consults this at the start of every agent run, so
//! configuration edits — e.g. adding an MCP server via the settings page —
//! take effect on the next run without restarting the process. A fingerprint
//! cache keeps rebuilding (which reconnects MCP servers) off the common path
//! where nothing changed: an unchanged configuration returns the same `Arc`,
//! and the shared registry keeps its MCP connections alive until the last run
//! using it ends.

use std::sync::Arc;
use tokio::sync::Mutex;

use crate::session::manager::ToolRegistryProvider;
use crate::tools::config::ToolsConfig;
use crate::tools::core::ToolRegistry;

struct Cached {
    fingerprint: String,
    registry: Arc<ToolRegistry>,
}

/// Rebuilds the tool registry from disk on demand, caching by a fingerprint of
/// the tool-relevant configuration files.
pub struct ConfigToolRegistry {
    cached: Mutex<Option<Cached>>,
}

impl ConfigToolRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cached: Mutex::new(None),
        })
    }

    /// The registry matching the current configuration — the cached one, or a
    /// freshly built one (reconnecting MCP servers) when `tools.json` or
    /// `mcp-servers.json` changed since the last call.
    pub async fn current(&self) -> Arc<ToolRegistry> {
        let fingerprint = Self::fingerprint();
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
        let registry = crate::tools::default_registry_with_mcp().await;
        *cached = Some(Cached {
            fingerprint,
            registry: registry.clone(),
        });
        registry
    }

    /// The provider closure for
    /// [`SessionManager::set_tool_registry_provider`](crate::session::manager::SessionManager::set_tool_registry_provider).
    pub fn as_provider(self: &Arc<Self>) -> ToolRegistryProvider {
        let this = self.clone();
        Arc::new(move || {
            let this = this.clone();
            Box::pin(async move { this.current().await })
        })
    }

    /// Fingerprint of the tool-relevant configuration files, read raw: equal
    /// fingerprint means equal registry input. Reading the files verbatim
    /// keeps `${VAR}` references unresolved, so a changed *environment* alone
    /// does not trigger a rebuild (matching process-startup semantics — the
    /// value is only re-read when the file itself changes).
    fn fingerprint() -> String {
        let read = |path: std::path::PathBuf| std::fs::read_to_string(path).unwrap_or_default();
        let tools = ToolsConfig::config_path().map(read).unwrap_or_default();
        let mcp = read(crate::tools::mcp::mcp_servers_config_path());
        format!("{tools}\u{0}{mcp}")
    }
}
