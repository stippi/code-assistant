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
//! gone. No connection a live run holds is ever shut down under it — except
//! by an explicit [restart](McpServerControl::restart).
//!
//! It also tracks how each server fared ([`McpServerHealth`]) and publishes
//! changes as app-scoped [`UiEvent::McpServerStatusChanged`] events.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use tokio::sync::Mutex;

use crate::session::event_stream::EventStream;
use crate::session::manager::{McpServerControl, RegistryRequest, ToolRegistryProvider};
use crate::tools::config::ToolsConfig;
use crate::tools::core::ToolRegistry;
use crate::ui::ui_events::{McpServerHealth, UiEvent};
use anyhow::Result;
use async_trait::async_trait;
use mcp_client::{ConnectionProvider, McpServerConfig, McpServerConnection, McpServerStatus};

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
    /// Locked only briefly, never across an await.
    connections: std::sync::Mutex<HashMap<String, Weak<McpServerConnection>>>,
    /// The last known state of each server that was connected (or tried).
    health: std::sync::Mutex<BTreeMap<String, McpServerHealth>>,
    /// Where status changes are published, once wired.
    events: OnceLock<EventStream>,
    /// Whether built registries include the
    /// [interactive tools](crate::tools::register_interactive_tools).
    interactive: bool,
}

impl ConfigToolRegistry {
    pub fn new() -> Arc<Self> {
        Self::build(false)
    }

    /// Like [`new`](Self::new), but the registries also offer the
    /// [interactive tools](crate::tools::register_interactive_tools) — for
    /// frontends that render them (GPUI).
    pub fn new_interactive() -> Arc<Self> {
        Self::build(true)
    }

    fn build(interactive: bool) -> Arc<Self> {
        Arc::new(Self {
            cached: Mutex::new(HashMap::new()),
            connections: std::sync::Mutex::new(HashMap::new()),
            health: std::sync::Mutex::new(BTreeMap::new()),
            events: OnceLock::new(),
            interactive,
        })
    }

    /// Publish server status changes to `events`.
    pub fn set_event_stream(&self, events: EventStream) {
        let _ = self.events.set(events);
    }

    /// The handle for
    /// [`SessionManager::set_mcp_server_control`](crate::session::manager::SessionManager::set_mcp_server_control).
    pub fn as_mcp_server_control(self: &Arc<Self>) -> Arc<dyn McpServerControl> {
        self.clone()
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
        let (registry, statuses) = crate::tools::registry_with_mcp_statuses(
            local_mcp_dir.as_deref(),
            self,
            self.interactive,
        )
        .await;
        self.record(&statuses);
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

    fn locked_health(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, McpServerHealth>> {
        self.health.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn locked_connections(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<String, Weak<McpServerConnection>>> {
        self.connections.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_health(&self, name: &str, health: McpServerHealth) {
        let changed = self
            .locked_health()
            .insert(name.to_string(), health.clone())
            != Some(health);
        if changed {
            self.publish();
        }
    }

    /// Take over the outcome of a registry build.
    fn record(&self, statuses: &[McpServerStatus]) {
        let mut changed = false;
        {
            let mut health = self.locked_health();
            for status in statuses {
                let new = match &status.result {
                    Ok(tools) => McpServerHealth::Connected { tools: tools.len() },
                    // The connect found the login missing; the status only
                    // carries the error text, so keep the more specific state.
                    Err(_) if health.get(&status.server) == Some(&McpServerHealth::NeedsLogin) => {
                        continue;
                    }
                    Err(error) => McpServerHealth::Failed {
                        error: error.clone(),
                    },
                };
                changed |= health.insert(status.server.clone(), new.clone()) != Some(new);
            }
        }
        if changed {
            self.publish();
        }
    }

    fn publish(&self) {
        if let Some(events) = self.events.get() {
            events.publish_app(UiEvent::McpServerStatusChanged {
                statuses: self.statuses(),
            });
        }
    }

    /// The live pooled connections of server `name`.
    fn live_connections(&self, name: &str) -> Vec<Arc<McpServerConnection>> {
        self.locked_connections()
            .iter()
            .filter(|(key, _)| server_of_key(key) == name)
            .filter_map(|(_, weak)| weak.upgrade())
            .collect()
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
        // Persisted OAuth tokens are an input to what an HTTP server
        // contributes: completing a login writes a token file but touches
        // none of the config files, so include it here or the cached
        // (pre-auth, tool-less) registry would be reused until restart.
        let oauth = crate::tools::mcp::mcp_oauth_fingerprint();
        // Installing, updating or switching a plugin changes its servers.
        let plugins = crate::plugin_host::catalog().fingerprint().to_string();
        let (dir, local) = local_mcp_dir
            .map(|dir| (dir.display().to_string(), read(dir.join(".mcp.json"))))
            .unwrap_or_default();
        format!("{tools}\u{0}{mcp}\u{0}{oauth}\u{0}{plugins}\u{0}{dir}\u{0}{local}")
    }
}

/// Identity of an MCP server for the connection pool: its name plus its
/// transport configuration. A changed transport (command, url, headers, …)
/// yields a different key, so the stale connection is not reused.
fn connection_key(name: &str, config: &McpServerConfig) -> String {
    let transport = serde_json::to_string(&config.transport).unwrap_or_default();
    format!("{name}\u{0}{transport}")
}

/// The server name a [`connection_key`] belongs to.
fn server_of_key(key: &str) -> &str {
    key.split('\u{0}').next().unwrap_or(key)
}

#[async_trait]
impl McpServerControl for ConfigToolRegistry {
    /// The recorded states, checked against the pool: a server whose
    /// connections all closed (its process exited) reads as failed, one that
    /// no registry holds any more as not connected.
    fn statuses(&self) -> BTreeMap<String, McpServerHealth> {
        let recorded = self.locked_health().clone();
        recorded
            .into_iter()
            .filter_map(|(name, health)| {
                if !matches!(health, McpServerHealth::Connected { .. }) {
                    return Some((name, health));
                }
                let live = self.live_connections(&name);
                if live.is_empty() {
                    None
                } else if live.iter().all(|connection| connection.is_closed()) {
                    Some((
                        name,
                        McpServerHealth::Failed {
                            error: "The connection to the server closed".to_string(),
                        },
                    ))
                } else {
                    Some((name, health))
                }
            })
            .collect()
    }

    async fn restart(&self, name: &str, req: RegistryRequest) -> Arc<ToolRegistry> {
        tracing::info!(server = name, "Restarting MCP server");
        let stale: Vec<_> = {
            let mut connections = self.locked_connections();
            let keys: Vec<String> = connections
                .keys()
                .filter(|key| server_of_key(key) == name)
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|key| connections.remove(&key))
                .filter_map(|weak| weak.upgrade())
                .collect()
        };
        for connection in stale {
            connection.close();
        }
        // Every cached registry may hold the closed connection (or lack the
        // failed server); the next run of any project rebuilds, reusing the
        // other servers from the pool.
        self.cached.lock().await.clear();
        self.set_health(name, McpServerHealth::Connecting);
        let registry = self.current_for(req).await;
        // Not part of this registry (e.g. switched off in the configuration,
        // or a local server of an untrusted project): nothing is connecting.
        let still_connecting = matches!(
            self.locked_health().get(name),
            Some(McpServerHealth::Connecting)
        );
        if still_connecting {
            self.locked_health().remove(name);
            self.publish();
        }
        registry
    }
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
            let mut connections = self.locked_connections();
            // Prune entries whose connection has died with its last registry.
            connections.retain(|_, weak| weak.strong_count() > 0);
            if let Some(existing) = connections.get(&key).and_then(Weak::upgrade) {
                return Ok(existing);
            }
        }
        self.set_health(name, McpServerHealth::Connecting);
        // Connect outside the lock (slow: process launch / HTTP handshake).
        // HTTP servers get a persistent OAuth credential store so a token
        // obtained via the interactive login is reused silently; stdio servers
        // have no OAuth, so pass none.
        let credentials = config
            .transport
            .is_http()
            .then(|| crate::tools::mcp::mcp_oauth_credential_store(name, config));
        let connection = match McpServerConnection::connect(name, config, credentials).await {
            // No login, or one that expired while the app was closed: an
            // expired one is renewed in the background, the tools appear on
            // the next run; a missing one waits for the user to log in.
            Err(error)
                if error
                    .downcast_ref::<mcp_client::AuthorizationRequired>()
                    .is_some() =>
            {
                crate::tools::mcp_auth::reauthorize_in_background(name, config);
                self.set_health(name, McpServerHealth::NeedsLogin);
                return Err(error);
            }
            Err(error) => {
                self.set_health(
                    name,
                    McpServerHealth::Failed {
                        error: format!("{error:#}"),
                    },
                );
                return Err(error);
            }
            Ok(connection) => connection,
        };
        // A login that expires while the connection is in use is renewed when
        // a tool call hits it.
        let connection = Arc::new(connection.with_reauthorizer(Arc::new(
            crate::tools::mcp_auth::AutoReauthorizer::new(config.clone()),
        )));
        // Re-check under the lock: if another caller connected the same server
        // meanwhile, keep theirs and drop ours (shut down on drop).
        let mut connections = self.locked_connections();
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
            off_in_new_sessions: false,
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
                let a = provider
                    .current_for(project_request(project_a.path()))
                    .await;
                let b = provider
                    .current_for(project_request(project_b.path()))
                    .await;
                assert!(
                    !Arc::ptr_eq(&a, &b),
                    "distinct projects, distinct registries"
                );

                // Alternating requests hit both caches — no thrash.
                let a2 = provider
                    .current_for(project_request(project_a.path()))
                    .await;
                let b2 = provider
                    .current_for(project_request(project_b.path()))
                    .await;
                assert!(
                    Arc::ptr_eq(&a, &a2),
                    "project A stays cached across B's build"
                );
                assert!(
                    Arc::ptr_eq(&b, &b2),
                    "project B stays cached across A's build"
                );
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

    /// Completing an OAuth login (a new/updated token file under the config
    /// dir's `mcp-oauth/`) rebuilds the registry, so the authorized server's
    /// tools appear on the next run without restarting the app.
    #[tokio::test]
    async fn oauth_token_change_rebuilds() {
        let config = tempfile::tempdir().unwrap();
        temp_env::async_with_vars(
            [("CODE_ASSISTANT_CONFIG_DIR", Some(config.path()))],
            async {
                let provider = ConfigToolRegistry::new();
                let before = provider.current().await;
                // Simulate the interactive login writing a token file.
                let oauth_dir = config.path().join("mcp-oauth");
                std::fs::create_dir_all(&oauth_dir).unwrap();
                std::fs::write(oauth_dir.join("sap_knowledge.json"), r#"{"client_id":"x"}"#)
                    .unwrap();
                let after = provider.current().await;
                assert!(
                    !Arc::ptr_eq(&before, &after),
                    "a new OAuth token must invalidate the cached registry"
                );
            },
        )
        .await;
    }

    fn health_events(
        subscription: &mut crate::session::event_stream::Subscription,
    ) -> Vec<BTreeMap<String, McpServerHealth>> {
        use futures::FutureExt;
        let mut seen = Vec::new();
        while let Some(Ok(event)) = subscription.recv().now_or_never() {
            if let crate::session::event_stream::EventPayload::Ui(
                UiEvent::McpServerStatusChanged { statuses },
            ) = event.payload
            {
                assert!(event.session_id.is_none(), "statuses are app-scoped");
                seen.push(statuses);
            }
        }
        seen
    }

    /// A server that cannot start reports its error, and a restart tries
    /// again: `Connecting` first, then the outcome.
    #[tokio::test]
    async fn failing_server_reports_its_error_and_restart_retries() {
        let config = tempfile::tempdir().unwrap();
        std::fs::write(
            config.path().join("mcp-servers.json"),
            r#"{ "servers": { "broken": { "command": "/surely/not/an/mcp-server" } } }"#,
        )
        .unwrap();
        temp_env::async_with_vars(
            [("CODE_ASSISTANT_CONFIG_DIR", Some(config.path()))],
            async {
                let provider = ConfigToolRegistry::new();
                let events = EventStream::new();
                let mut subscription = events.subscribe();
                provider.set_event_stream(events);

                let _registry = provider.current().await;
                let Some(McpServerHealth::Failed { error }) = provider.statuses().remove("broken")
                else {
                    panic!("expected a failure, got {:?}", provider.statuses());
                };
                assert!(error.contains("failed to launch"), "{error}");
                health_events(&mut subscription);

                let before = provider.current().await;
                let after = provider.restart("broken", RegistryRequest::default()).await;
                assert!(
                    !Arc::ptr_eq(&before, &after),
                    "a restart rebuilds the registry"
                );
                let states: Vec<_> = health_events(&mut subscription)
                    .into_iter()
                    .map(|statuses| statuses["broken"].clone())
                    .collect();
                assert_eq!(states.first(), Some(&McpServerHealth::Connecting));
                assert!(matches!(
                    states.last(),
                    Some(McpServerHealth::Failed { .. })
                ));
            },
        )
        .await;
    }

    /// An HTTP server from a project's `.mcp.json` that demands OAuth, with
    /// no login yet, waits for one (rather than reading as a plain failure),
    /// and no browser opens on its own.
    #[tokio::test]
    async fn server_without_login_needs_one() {
        use axum::http::StatusCode;

        let router = axum::Router::new().route(
            "/mcp",
            axum::routing::any(|| async {
                (
                    StatusCode::UNAUTHORIZED,
                    [(
                        "WWW-Authenticate",
                        "Bearer resource_metadata=\"https://auth.example.com/.well-known/oauth-protected-resource\"",
                    )],
                    "authorization required",
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        let config = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join(".mcp.json"),
            format!(r#"{{ "mcpServers": {{ "remote": {{ "type": "http", "url": "http://{addr}/mcp" }} }} }}"#),
        )
        .unwrap();
        temp_env::async_with_vars(
            [("CODE_ASSISTANT_CONFIG_DIR", Some(config.path()))],
            async {
                let provider = ConfigToolRegistry::new();
                let _registry = provider
                    .current_for(RegistryRequest {
                        project_dir: Some(project.path().to_path_buf()),
                        include_local_mcp: true,
                    })
                    .await;
                assert_eq!(
                    provider.statuses().remove("remote"),
                    Some(McpServerHealth::NeedsLogin)
                );
                assert!(
                    std::fs::read_dir(crate::tools::mcp::mcp_oauth_dir())
                        .map_or(true, |mut entries| entries.next().is_none())
                );
            },
        )
        .await;
        server.abort();
    }

    /// A server skipped while loading the configuration (an unresolvable
    /// `${VAR}`) reads as failed with the reason, not as not connected.
    #[tokio::test]
    async fn skipped_server_reports_why() {
        let config = tempfile::tempdir().unwrap();
        std::fs::write(
            config.path().join("mcp-servers.json"),
            r#"{ "servers": { "needs-secret": {
                "command": "/surely/not/an/mcp-server",
                "env": { "TOKEN": "${MCP_TEST_SURELY_UNSET}" }
            } } }"#,
        )
        .unwrap();
        temp_env::async_with_vars(
            [("CODE_ASSISTANT_CONFIG_DIR", Some(config.path()))],
            async {
                let provider = ConfigToolRegistry::new();
                let _registry = provider.current().await;
                let Some(McpServerHealth::Failed { error }) =
                    provider.statuses().remove("needs-secret")
                else {
                    panic!("expected a failure, got {:?}", provider.statuses());
                };
                assert!(error.contains("MCP_TEST_SURELY_UNSET"), "{error}");
            },
        )
        .await;
    }

    /// Restarting a server the registry does not include (e.g. switched off
    /// in the configuration) does not leave it "connecting".
    #[tokio::test]
    async fn restarting_an_unknown_server_settles() {
        let config = tempfile::tempdir().unwrap();
        temp_env::async_with_vars(
            [("CODE_ASSISTANT_CONFIG_DIR", Some(config.path()))],
            async {
                let provider = ConfigToolRegistry::new();
                let _registry = provider.restart("gone", RegistryRequest::default()).await;
                assert!(provider.statuses().is_empty());
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
