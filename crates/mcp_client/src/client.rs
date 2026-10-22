//! Connection to a single MCP server, built on the official rmcp SDK.
//!
//! One connection per configured server. For a stdio server the child process
//! lives as long as the connection; for an HTTP server it is a streamable HTTP
//! session. Wrapped tools hold the connection behind an `Arc`, so a dead
//! server degrades to tool errors, never a crashed agent.
//!
//! HTTP servers may require OAuth (MCP's authorization spec): connecting is
//! *reactive* — we try unauthenticated first and, on the server's `401`
//! challenge, either reuse a stored token non-interactively or surface an
//! [`AuthorizationRequired`] error so the embedder can offer an interactive
//! login. That login is [`authenticate_http_server`], driven by an
//! [`OAuthAuthorizer`]; it persists tokens into the given [`CredentialStore`]
//! so later connects reuse them silently.
//!
//! A login can also expire while a connection is in use (the server rejects
//! the token and refreshing it fails). A call that fails at the transport then
//! reconnects; when the server demands authorization again, the connection
//! asks the embedder's [`Reauthorizer`] for fresh tokens, reconnects once more
//! and retries the call.

use crate::auth::{AuthorizationRequired, OAuthAuthorizer, Reauthorizer, SharedCredentialStore};
use crate::config::{McpServerConfig, McpTransport};
use anyhow::{Context, Result, anyhow};
use rmcp::ServiceExt;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, JsonObject, ReadResourceRequestParams,
    ReadResourceResult, Resource, ResourceTemplate, Tool as McpToolDescriptor,
};
use rmcp::service::{RoleClient, RunningService, ServiceError};
use rmcp::transport::IntoTransport;
use rmcp::transport::auth::{
    AuthClient, AuthorizationManager, AuthorizationRequest, AuthorizationSession, CredentialStore,
};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Timeout for the initialize handshake and for listing tools and resources.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout for a single tool call or resource read round-trip. Generous: MCP
/// tools may do real work (searches, API calls), but a hung server must not
/// hang a turn forever.
const CALL_TIMEOUT: Duration = Duration::from_secs(300);
/// Timeout for OAuth HTTP operations (discovery, registration, token exchange
/// and refresh). Kept off the transport client so it never cuts long-lived
/// SSE streams short.
const OAUTH_HTTP_TIMEOUT: Duration = Duration::from_secs(30);

type Service = RunningService<RoleClient, ()>;

/// A live connection to one MCP server.
pub struct McpServerConnection {
    name: String,
    /// The live session; replaced when an HTTP server is reconnected. Calls
    /// in flight keep the session they started on.
    service: RwLock<Arc<Service>>,
    /// How to open the session again: HTTP servers only.
    reconnect: Option<Reconnect>,
    /// Held while recovering, so concurrent failing calls reconnect once.
    recovering: tokio::sync::Mutex<()>,
}

/// What reopening an HTTP session needs.
struct Reconnect {
    url: String,
    headers: HashMap<String, String>,
    credentials: Option<Arc<dyn CredentialStore>>,
    reauthorizer: Option<Arc<dyn Reauthorizer>>,
}

/// How a failed call's connection was recovered.
enum Recovery {
    /// A new session with the credentials the store already held, or one
    /// another call opened meanwhile.
    Reconnected,
    /// A new session after the [`Reauthorizer`] obtained fresh tokens.
    Reauthorized,
}

/// What a request does, deciding its timeout and whether it may be sent
/// again after its session failed.
#[derive(Clone, Copy)]
enum Kind {
    /// `tools/call`: may have run before the session failed.
    ToolCall,
    /// `*/list`: side-effect free.
    Listing,
    /// `resources/read`: side-effect free.
    Read,
}

impl Kind {
    fn timeout(self) -> Duration {
        match self {
            Kind::Listing => CONNECT_TIMEOUT,
            Kind::ToolCall | Kind::Read => CALL_TIMEOUT,
        }
    }

    fn is_side_effect_free(self) -> bool {
        matches!(self, Kind::Listing | Kind::Read)
    }
}

/// Why a single request attempt failed.
enum CallError {
    Timeout,
    Service(ServiceError),
}

impl McpServerConnection {
    /// Connect to the configured server, running the MCP initialize handshake
    /// over its transport, reusing OAuth tokens from `credentials` for an HTTP
    /// server when present.
    ///
    /// Pass `None` when there is nothing to authenticate with (stdio servers,
    /// which have no OAuth; or a server reached purely via a static
    /// `Authorization` header). With `None`, or when the store holds no valid
    /// token, an HTTP server that demands OAuth fails with a typed
    /// [`AuthorizationRequired`] error — this never opens a browser, so the
    /// caller (or a UI) can offer the interactive login
    /// ([`authenticate_http_server`]) instead. With a valid stored token the
    /// connection is authorized silently (rmcp refreshes it transparently).
    pub async fn connect(
        name: &str,
        config: &McpServerConfig,
        credentials: Option<Arc<dyn CredentialStore>>,
    ) -> Result<Self> {
        match &config.transport {
            McpTransport::Stdio { command, args, env } => {
                Self::connect_stdio(name, command, args, env).await
            }
            McpTransport::Http { url, headers } => {
                let service = Self::open_http(name, url, headers, credentials.clone()).await?;
                let mut connection = Self::from_service(name, service);
                connection.reconnect = Some(Reconnect {
                    url: url.clone(),
                    headers: headers.clone(),
                    credentials,
                    reauthorizer: None,
                });
                Ok(connection)
            }
        }
    }

    /// Re-authorize through `reauthorizer` when the server's OAuth login
    /// expires while this connection is in use. Only HTTP servers can expire;
    /// for others this is a no-op.
    pub fn with_reauthorizer(mut self, reauthorizer: Arc<dyn Reauthorizer>) -> Self {
        if let Some(reconnect) = &mut self.reconnect {
            reconnect.reauthorizer = Some(reauthorizer);
        }
        self
    }

    fn from_service(name: &str, service: Service) -> Self {
        Self {
            name: name.to_string(),
            service: RwLock::new(Arc::new(service)),
            reconnect: None,
            recovering: tokio::sync::Mutex::new(()),
        }
    }

    fn current(&self) -> Arc<Service> {
        self.service
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Launch `command` as a child process and run the MCP initialize
    /// handshake over its stdio.
    async fn connect_stdio(
        name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<Self> {
        let mut process = tokio::process::Command::new(command);
        process.args(args).envs(env);
        let transport = rmcp::transport::child_process::TokioChildProcess::new(process)
            .with_context(|| format!("failed to launch MCP server '{name}' ({command})"))?;
        Self::connect_transport(name, transport).await
    }

    /// Open a session with an HTTP (streamable) MCP server at `url`, sending
    /// the given custom headers (e.g. a static `Authorization`) with every
    /// request, and reusing OAuth tokens from `credentials` when available.
    async fn open_http(
        name: &str,
        url: &str,
        headers: &HashMap<String, String>,
        credentials: Option<Arc<dyn CredentialStore>>,
    ) -> Result<Service> {
        // 1. If we hold a stored OAuth token, connect authorized without any
        //    user interaction (rmcp refreshes it transparently when needed).
        if let Some(store) = &credentials
            && let Some(service) =
                Self::connect_http_with_stored_token(name, url, headers, store.clone()).await?
        {
            return Ok(service);
        }

        // 2. Try unauthenticated (also the path for a static `Authorization`
        //    header). A 401 surfaces as a typed AuthorizationRequired.
        let transport =
            StreamableHttpClientTransport::from_config(http_transport_config(url, headers)?);
        Self::serve_http(name, transport)
            .await
            .with_context(|| format!("failed to connect to HTTP MCP server '{name}' ({url})"))
    }

    /// Try to connect with a previously stored OAuth token. Returns `Ok(None)`
    /// when the store holds no usable credentials — none at all, or a login
    /// the server no longer accepts (e.g. the refresh failed) — so the caller
    /// falls back to an unauthenticated attempt, whose `401` reports that a
    /// login is needed. Only a timeout is passed on: the server is not
    /// answering, and a second attempt would just wait again.
    async fn connect_http_with_stored_token(
        name: &str,
        url: &str,
        headers: &HashMap<String, String>,
        store: Arc<dyn CredentialStore>,
    ) -> Result<Option<RunningService<RoleClient, ()>>> {
        let mut manager = AuthorizationManager::new(url).await.map_err(|error| {
            anyhow!("initializing OAuth manager for MCP server '{name}': {error}")
        })?;
        manager.set_credential_store(SharedCredentialStore(store));
        manager.with_client(oauth_http_client()?).map_err(|error| {
            anyhow!("configuring OAuth client for MCP server '{name}': {error}")
        })?;

        let has_credentials = manager
            .initialize_from_store()
            .await
            .map_err(|error| anyhow!("loading stored credentials for '{name}': {error}"))?;
        if !has_credentials {
            return Ok(None);
        }

        let auth_client = AuthClient::new(reqwest::Client::new(), manager);
        let transport = StreamableHttpClientTransport::with_client(
            auth_client,
            http_transport_config(url, headers)?,
        );
        match Self::serve_http(name, transport).await {
            Ok(service) => Ok(Some(service)),
            Err(error) if error.is::<InitializeTimeout>() => Err(error.context(format!(
                "failed to connect to HTTP MCP server '{name}' ({url}) with stored credentials"
            ))),
            Err(error) => {
                tracing::warn!(
                    "MCP server '{name}' did not accept the stored login, trying without: {error:#}"
                );
                Ok(None)
            }
        }
    }

    /// Serve the client handler over a streamable HTTP transport, mapping the
    /// server's `401` authorization challenge to a typed
    /// [`AuthorizationRequired`] error and everything else to a plain failure.
    async fn serve_http<T, E, A>(name: &str, transport: T) -> Result<RunningService<RoleClient, ()>>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        match tokio::time::timeout(CONNECT_TIMEOUT, ().serve(transport)).await {
            Err(_elapsed) => Err(anyhow::Error::new(InitializeTimeout(name.to_string()))),
            Ok(Ok(service)) => Ok(service),
            Ok(Err(error)) => match error.auth_challenge() {
                Some(challenge) => Err(anyhow::Error::new(AuthorizationRequired {
                    server: name.to_string(),
                    challenge: challenge.to_string(),
                })),
                None => Err(anyhow::Error::new(error)
                    .context(format!("failed to initialize MCP server '{name}'"))),
            },
        }
    }

    /// Run the MCP initialize handshake over an arbitrary transport. Used by
    /// stdio servers and by tests (in-process duplex streams); HTTP servers go
    /// through [`Self::connect_http`] so they get OAuth handling.
    pub async fn connect_transport<T, E, A>(name: &str, transport: T) -> Result<Self>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let service = tokio::time::timeout(CONNECT_TIMEOUT, ().serve(transport))
            .await
            .with_context(|| format!("timeout initializing MCP server '{name}'"))?
            .with_context(|| format!("failed to initialize MCP server '{name}'"))?;
        Ok(Self::from_service(name, service))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// All tools the server offers (follows pagination).
    pub async fn list_tools(&self) -> Result<Vec<McpToolDescriptor>> {
        tokio::time::timeout(CONNECT_TIMEOUT, self.current().list_all_tools())
            .await
            .with_context(|| format!("timeout listing tools of MCP server '{}'", self.name))?
            .with_context(|| format!("failed to list tools of MCP server '{}'", self.name))
    }

    /// Whether the server declared the `resources` capability in its
    /// initialize handshake.
    pub fn supports_resources(&self) -> bool {
        self.current()
            .peer_info()
            .is_some_and(|info| info.capabilities.resources.is_some())
    }

    /// All resources the server lists (follows pagination), recovering the
    /// session like [`Self::request`].
    pub async fn list_resources(&self) -> Result<Vec<Resource>> {
        self.request("listing resources", Kind::Listing, |service| async move {
            service.list_all_resources().await
        })
        .await
    }

    /// All resource templates the server lists (follows pagination),
    /// recovering the session like [`Self::request`].
    pub async fn list_resource_templates(&self) -> Result<Vec<ResourceTemplate>> {
        self.request(
            "listing resource templates",
            Kind::Listing,
            |service| async move { service.list_all_resource_templates().await },
        )
        .await
    }

    /// Round-trip a `tools/call` request, recovering the session like
    /// [`Self::request`].
    pub async fn call_tool(
        &self,
        tool: &str,
        arguments: Option<JsonObject>,
    ) -> Result<CallToolResult> {
        self.request(&format!("tool '{tool}'"), Kind::ToolCall, |service| {
            let mut params = CallToolRequestParams::new(tool.to_string());
            params.arguments = arguments.clone();
            async move { service.call_tool(params).await }
        })
        .await
    }

    /// Round-trip a `resources/read` request, recovering the session like
    /// [`Self::request`].
    pub async fn read_resource(&self, uri: &str) -> Result<ReadResourceResult> {
        self.request(
            &format!("reading resource '{uri}'"),
            Kind::Read,
            |service| {
                let params = ReadResourceRequestParams::new(uri);
                async move { service.read_resource(params).await }
            },
        )
        .await
    }

    /// Send one request (`what` names it in errors). When an HTTP server's
    /// session fails at the transport, the connection is reopened
    /// (re-authorizing if the login expired); the request is sent again when
    /// that is safe for its [`Kind`].
    async fn request<T, F, Fut>(&self, what: &str, kind: Kind, send: F) -> Result<T>
    where
        F: Fn(Arc<Service>) -> Fut,
        Fut: Future<Output = Result<T, ServiceError>>,
    {
        let service = self.current();
        let error = match Self::send_on(service.clone(), kind.timeout(), &send).await {
            Ok(result) => return Ok(result),
            Err(CallError::Service(error)) if self.reconnect.is_some() && is_transport(&error) => {
                error
            }
            Err(error) => return Err(self.request_error(what, error)),
        };
        // A request the server rejected for authorization cannot have run.
        let resend = kind.is_side_effect_free() || rejected_for_authorization(&error);
        match self.recover(&service).await {
            Ok(Recovery::Reauthorized) => {}
            Ok(Recovery::Reconnected) if resend => {}
            Ok(Recovery::Reconnected) => {
                return Err(self
                    .request_error(what, CallError::Service(error))
                    .context("reconnected to the server; the call can be retried"));
            }
            Err(recovery) => {
                return Err(recovery.context(format!(
                    "{what} failed on MCP server '{}' ({error})",
                    self.name
                )));
            }
        }
        Self::send_on(self.current(), kind.timeout(), &send)
            .await
            .map_err(|error| self.request_error(what, error))
    }

    async fn send_on<T, F, Fut>(
        service: Arc<Service>,
        timeout: Duration,
        send: &F,
    ) -> Result<T, CallError>
    where
        F: Fn(Arc<Service>) -> Fut,
        Fut: Future<Output = Result<T, ServiceError>>,
    {
        match tokio::time::timeout(timeout, send(service)).await {
            Err(_elapsed) => Err(CallError::Timeout),
            Ok(result) => result.map_err(CallError::Service),
        }
    }

    fn request_error(&self, what: &str, error: CallError) -> anyhow::Error {
        match error {
            CallError::Timeout => anyhow!("timeout: {what} on MCP server '{}'", self.name),
            CallError::Service(error) => anyhow::Error::new(error)
                .context(format!("{what} failed on MCP server '{}'", self.name)),
        }
    }

    /// Reopen the session `failed` ran on, re-authorizing when the server
    /// demands it. Only for HTTP servers (`reconnect` is set).
    async fn recover(&self, failed: &Arc<Service>) -> Result<Recovery> {
        let reconnect = self
            .reconnect
            .as_ref()
            .context("this MCP server cannot reconnect")?;
        let _recovering = self.recovering.lock().await;
        if !Arc::ptr_eq(&self.current(), failed) {
            // Another call recovered while this one waited.
            return Ok(Recovery::Reconnected);
        }
        let open = || {
            Self::open_http(
                &self.name,
                &reconnect.url,
                &reconnect.headers,
                reconnect.credentials.clone(),
            )
        };
        let recovery = match open().await {
            Ok(service) => {
                self.replace(service);
                Recovery::Reconnected
            }
            Err(error) if error.downcast_ref::<AuthorizationRequired>().is_some() => {
                let Some(reauthorizer) = &reconnect.reauthorizer else {
                    return Err(error);
                };
                tracing::info!(
                    server = self.name,
                    "MCP server login expired; re-authorizing"
                );
                reauthorizer.reauthorize(&self.name).await?;
                self.replace(open().await?);
                Recovery::Reauthorized
            }
            Err(error) => return Err(error),
        };
        Ok(recovery)
    }

    fn replace(&self, service: Service) {
        *self.service.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(service);
    }

    /// Whether the session has ended: cancelled through [`Self::close`], or
    /// the transport went away (a stdio server's process exited, an HTTP
    /// session dropped). An HTTP connection still reopens on its next call.
    pub fn is_closed(&self) -> bool {
        let service = self.current();
        service.is_closed() || service.peer().is_transport_closed()
    }

    /// Stop the session without owning the connection, terminating a stdio
    /// server's child process. Calls still running on it fail; registries
    /// holding the connection keep a dead one until they are rebuilt.
    pub fn close(&self) {
        self.current().cancellation_token().cancel();
    }

    /// Close the connection, terminating the server child process. Dropping
    /// the connection has the same effect; this form allows awaiting it.
    pub async fn shutdown(self) -> Result<()> {
        let service = self.service.into_inner().unwrap_or_else(|e| e.into_inner());
        match Arc::try_unwrap(service) {
            Ok(service) => service.cancel().await.map(|_| ()).map_err(|e| {
                anyhow::anyhow!("failed to shut down MCP server '{}': {e}", self.name)
            }),
            // A call still runs on it: stop the session under it.
            Err(shared) => {
                shared.cancellation_token().cancel();
                Ok(())
            }
        }
    }
}

/// The server did not complete the MCP initialize handshake in time.
#[derive(Debug, thiserror::Error)]
#[error("timeout initializing MCP server '{0}'")]
struct InitializeTimeout(String);

/// The session itself failed (sending, or the transport closed), as opposed
/// to the server answering with an error or the call timing out.
fn is_transport(error: &ServiceError) -> bool {
    matches!(
        error,
        ServiceError::TransportSend(_) | ServiceError::TransportClosed
    )
}

/// The server rejected the request for missing or expired authorization
/// (`401`/`403` with a challenge, or no token could be obtained).
fn rejected_for_authorization(error: &ServiceError) -> bool {
    use rmcp::transport::auth::AuthError;
    use rmcp::transport::streamable_http_client::{AuthRequiredError, InsufficientScopeError};

    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = source {
        if current.is::<AuthRequiredError>()
            || current.is::<InsufficientScopeError>()
            || matches!(
                current.downcast_ref::<AuthError>(),
                Some(AuthError::AuthorizationRequired)
            )
        {
            return true;
        }
        source = current.source();
    }
    false
}

/// Build the streamable HTTP transport config for `url`, attaching any custom
/// headers (values already have `${VAR}` substituted by the config layer).
fn http_transport_config(
    url: &str,
    headers: &HashMap<String, String>,
) -> Result<StreamableHttpClientTransportConfig> {
    let mut config = StreamableHttpClientTransportConfig::with_uri(url.to_string());
    if !headers.is_empty() {
        let mut header_map = HashMap::with_capacity(headers.len());
        for (key, value) in headers {
            let name = http::HeaderName::from_bytes(key.as_bytes())
                .with_context(|| format!("invalid HTTP header name '{key}'"))?;
            let value = http::HeaderValue::from_str(value)
                .with_context(|| format!("invalid value for HTTP header '{key}'"))?;
            header_map.insert(name, value);
        }
        config = config.custom_headers(header_map);
    }
    Ok(config)
}

/// A `reqwest` client for OAuth HTTP operations (discovery, registration,
/// token exchange, refresh) with a bounded timeout.
fn oauth_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(OAUTH_HTTP_TIMEOUT)
        .build()
        .context("building OAuth HTTP client")
}

/// Run the interactive OAuth login for an HTTP MCP server and persist the
/// resulting tokens into `credential_store`, so a later
/// [`McpServerConnection::connect`] reuses them without any user interaction.
///
/// The flow: discover the authorization server from `config`'s URL, register
/// (or select) an OAuth client, hand the authorization URL to `authorizer`
/// (which opens a browser and waits for the redirect callback), then exchange
/// the returned code for tokens. `client_name` labels this client during
/// dynamic client registration.
///
/// Errors if `config` is not an HTTP server — OAuth does not apply to stdio.
pub async fn authenticate_http_server(
    name: &str,
    config: &McpServerConfig,
    credential_store: Arc<dyn CredentialStore>,
    authorizer: &dyn OAuthAuthorizer,
    client_name: &str,
) -> Result<()> {
    let McpTransport::Http { url, .. } = &config.transport else {
        return Err(anyhow!(
            "MCP server '{name}' is not an HTTP server; OAuth authorization only applies to HTTP transports"
        ));
    };

    let mut manager = AuthorizationManager::new(url)
        .await
        .map_err(|error| anyhow!("initializing OAuth manager for MCP server '{name}': {error}"))?;
    manager.set_credential_store(SharedCredentialStore(credential_store));
    manager
        .with_client(oauth_http_client()?)
        .map_err(|error| anyhow!("configuring OAuth client for MCP server '{name}': {error}"))?;

    // Discover the authorization server (RFC 9728 → RFC 8414 / OIDC).
    let resolution = manager
        .resolve_metadata()
        .await
        .map_err(|error| anyhow!("discovering OAuth metadata for MCP server '{name}': {error}"))?;
    manager.set_metadata(resolution.metadata);

    // Select a client (pre-registered / CIMD / dynamic registration) and build
    // the authorization URL.
    let request =
        AuthorizationRequest::new(authorizer.redirect_uri()).with_client_name(client_name);
    let session =
        AuthorizationSession::new(manager, request)
            .await
            .map_err(|(_manager, error)| {
                anyhow!("starting OAuth authorization for MCP server '{name}': {error}")
            })?;

    // Hand the URL to the embedder: open a browser and await the redirect.
    let outcome = authorizer
        .authorize(session.get_authorization_url().to_string())
        .await
        .with_context(|| format!("browser authorization for MCP server '{name}'"))?;

    // Exchange the code for tokens; this persists StoredCredentials into the
    // shared credential store as a side effect.
    session
        .handle_callback_with_issuer(&outcome.code, &outcome.state, outcome.issuer.as_deref())
        .await
        .map_err(|error| {
            anyhow!("exchanging OAuth authorization code for MCP server '{name}': {error}")
        })?;

    Ok(())
}
