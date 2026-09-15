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

use crate::auth::{AuthorizationRequired, OAuthAuthorizer, SharedCredentialStore};
use crate::config::{McpServerConfig, McpTransport};
use anyhow::{Context, Result, anyhow};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, JsonObject, Tool as McpToolDescriptor};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::IntoTransport;
use rmcp::transport::auth::{
    AuthClient, AuthorizationManager, AuthorizationRequest, AuthorizationSession, CredentialStore,
};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Timeout for the initialize handshake and for tool discovery.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout for a single tool call round-trip. Generous: MCP tools may do
/// real work (searches, API calls), but a hung server must not hang a turn
/// forever.
const CALL_TIMEOUT: Duration = Duration::from_secs(300);
/// Timeout for OAuth HTTP operations (discovery, registration, token exchange
/// and refresh). Kept off the transport client so it never cuts long-lived
/// SSE streams short.
const OAUTH_HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// A live connection to one MCP server.
pub struct McpServerConnection {
    name: String,
    service: RunningService<RoleClient, ()>,
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
                Self::connect_http(name, url, headers, credentials).await
            }
        }
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

    /// Connect to an HTTP (streamable) MCP server at `url`, sending the given
    /// custom headers (e.g. a static `Authorization`) with every request, and
    /// reusing OAuth tokens from `credentials` when available.
    async fn connect_http(
        name: &str,
        url: &str,
        headers: &HashMap<String, String>,
        credentials: Option<Arc<dyn CredentialStore>>,
    ) -> Result<Self> {
        // 1. If we hold a stored OAuth token, connect authorized without any
        //    user interaction (rmcp refreshes it transparently when needed).
        if let Some(store) = &credentials
            && let Some(service) =
                Self::connect_http_with_stored_token(name, url, headers, store.clone()).await?
        {
            return Ok(Self {
                name: name.to_string(),
                service,
            });
        }

        // 2. Try unauthenticated (also the path for a static `Authorization`
        //    header). A 401 surfaces as a typed AuthorizationRequired.
        let transport =
            StreamableHttpClientTransport::from_config(http_transport_config(url, headers)?);
        Self::serve_http(name, transport)
            .await
            .map(|service| Self {
                name: name.to_string(),
                service,
            })
            .with_context(|| format!("failed to connect to HTTP MCP server '{name}' ({url})"))
    }

    /// Try to connect with a previously stored OAuth token. Returns `Ok(None)`
    /// when the store holds no usable credentials, so the caller falls back to
    /// an unauthenticated attempt.
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
        Self::serve_http(name, transport)
            .await
            .map(Some)
            .with_context(|| {
                format!(
                    "failed to connect to HTTP MCP server '{name}' ({url}) with stored credentials"
                )
            })
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
            Err(_elapsed) => Err(anyhow!("timeout initializing MCP server '{name}'")),
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
        Ok(Self {
            name: name.to_string(),
            service,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// All tools the server offers (follows pagination).
    pub async fn list_tools(&self) -> Result<Vec<McpToolDescriptor>> {
        tokio::time::timeout(CONNECT_TIMEOUT, self.service.list_all_tools())
            .await
            .with_context(|| format!("timeout listing tools of MCP server '{}'", self.name))?
            .with_context(|| format!("failed to list tools of MCP server '{}'", self.name))
    }

    /// Round-trip a `tools/call` request.
    pub async fn call_tool(
        &self,
        tool: &str,
        arguments: Option<JsonObject>,
    ) -> Result<CallToolResult> {
        let mut params = CallToolRequestParams::new(tool.to_string());
        params.arguments = arguments;
        tokio::time::timeout(CALL_TIMEOUT, self.service.call_tool(params))
            .await
            .with_context(|| {
                format!(
                    "timeout calling tool '{tool}' on MCP server '{}'",
                    self.name
                )
            })?
            .with_context(|| format!("tool '{tool}' failed on MCP server '{}'", self.name))
    }

    /// Close the connection, terminating the server child process. Dropping
    /// the connection has the same effect; this form allows awaiting it.
    pub async fn shutdown(self) -> Result<()> {
        self.service
            .cancel()
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("failed to shut down MCP server '{}': {e}", self.name))
    }
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
