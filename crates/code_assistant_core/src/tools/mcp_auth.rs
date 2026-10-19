//! Interactive OAuth login for HTTP MCP servers (MCP's authorization spec).
//!
//! [`LoopbackAuthorizer`] implements the [`OAuthAuthorizer`] seam with the
//! RFC 8252 native-app pattern: a one-shot loopback HTTP server receives the
//! OAuth redirect while the user's browser is opened to the authorization
//! URL. [`login_mcp_server`] wires it to a configured server and persists the
//! resulting tokens, so later agent runs connect silently.
//!
//! Shared by the CLI (`mcp-login`) and the settings UI so both drive the same
//! flow.
//!
//! [`AutoReauthorizer`] runs that same login on its own when a server's login
//! expired, if `auto_reauthorize` is on in `mcp-servers.json`.

use super::mcp::{self, AuthorizationOutcome, McpServerConfig, OAuthAuthorizer};
use anyhow::{Context, Result};
use futures::FutureExt as _;
use futures::future::{BoxFuture, Shared};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;

/// An [`OAuthAuthorizer`] that runs a one-shot loopback HTTP server for the
/// OAuth redirect and opens the user's browser to the authorization URL.
pub struct LoopbackAuthorizer {
    redirect_uri: String,
    /// Bound at construction so [`Self::redirect_uri`] reflects the real port;
    /// consumed by the single `authorize` call.
    listener: Mutex<Option<TcpListener>>,
}

impl LoopbackAuthorizer {
    /// Bind an ephemeral loopback port for the redirect callback.
    pub async fn bind() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("binding the OAuth callback server on loopback")?;
        let port = listener.local_addr()?.port();
        Ok(Self {
            redirect_uri: format!("http://127.0.0.1:{port}/callback"),
            listener: Mutex::new(Some(listener)),
        })
    }
}

#[async_trait::async_trait]
impl OAuthAuthorizer for LoopbackAuthorizer {
    fn redirect_uri(&self) -> String {
        self.redirect_uri.clone()
    }

    async fn authorize(&self, authorization_url: String) -> Result<AuthorizationOutcome> {
        let listener = self
            .listener
            .lock()
            .expect("authorizer mutex poisoned")
            .take()
            .context("the loopback authorizer can only authorize once")?;

        // Log and print the URL so it is reachable even if the browser does
        // not open (headless, or a GPUI settings screen without a console).
        tracing::info!("MCP OAuth authorization URL: {authorization_url}");
        eprintln!("Authorize this MCP server in your browser:\n  {authorization_url}");
        if let Err(e) = open::that(&authorization_url) {
            tracing::warn!("Could not open the browser automatically: {e}");
        }

        let (mut stream, _) = tokio::time::timeout(Duration::from_secs(300), listener.accept())
            .await
            .context("authorization timed out after 5 minutes")?
            .context("accepting the OAuth callback connection")?;

        let params = read_callback_query(&mut stream).await?;

        if let Some(error) = params.get("error") {
            let description = params.get("error_description").cloned().unwrap_or_default();
            respond(
                &mut stream,
                "400 Bad Request",
                "Authorization failed",
                "You can close this window and return to the app.",
            )
            .await;
            anyhow::bail!("authorization server returned an error: {error} {description}");
        }

        let code = params
            .get("code")
            .cloned()
            .context("the callback did not include an authorization code")?;
        let state = params.get("state").cloned().unwrap_or_default();
        let issuer = params.get("iss").cloned();

        respond(
            &mut stream,
            "200 OK",
            "Authorization complete",
            "You can close this window and return to the app.",
        )
        .await;

        Ok(AuthorizationOutcome {
            code,
            state,
            issuer,
        })
    }
}

/// Run the OAuth browser login for the HTTP MCP server `name` and persist its
/// tokens. The server is looked up in the global `mcp-servers.json` and, when
/// `local_mcp_dir` is given, the project-local `.mcp.json` there (see
/// [`mcp::resolve_mcp_server`]).
pub async fn login_mcp_server(name: &str, local_mcp_dir: Option<&Path>) -> Result<()> {
    let server = mcp::resolve_mcp_server(name, local_mcp_dir)?;
    login_configured_server(name, &server).await
}

/// Run the OAuth browser login for the HTTP MCP server `name`, configured as
/// `server`, and persist its tokens. Binds a [`LoopbackAuthorizer`] to
/// [`mcp::authenticate_mcp_server`].
pub async fn login_configured_server(name: &str, server: &McpServerConfig) -> Result<()> {
    let authorizer = LoopbackAuthorizer::bind().await?;
    mcp::authenticate_mcp_server(name, server, &authorizer).await
}

/// Logs an HTTP MCP server in again when its OAuth login expired, if
/// `auto_reauthorize` is on in `mcp-servers.json`. The setting is read on
/// every use, so switching it in the settings applies at once.
///
/// Holds the server's configuration rather than looking it up by name, so a
/// server from a project's `.mcp.json` is renewed as well.
pub struct AutoReauthorizer {
    server: McpServerConfig,
}

impl AutoReauthorizer {
    pub fn new(server: McpServerConfig) -> Self {
        Self { server }
    }
}

#[async_trait::async_trait]
impl mcp_client::Reauthorizer for AutoReauthorizer {
    async fn reauthorize(&self, server: &str) -> Result<()> {
        if !mcp::has_mcp_oauth_tokens(server) {
            anyhow::bail!("MCP server '{server}' requires authorization");
        }
        if !auto_reauthorize_enabled() {
            anyhow::bail!(
                "the login of MCP server '{server}' expired and automatic re-authorization \
                 is off; log in again in the MCP server settings (or run \
                 `code-assistant mcp-login {server}`)"
            );
        }
        relogin(server, &self.server)
            .await
            .map_err(anyhow::Error::msg)
    }
}

/// Whether expired logins are renewed automatically (on unless switched off).
pub fn auto_reauthorize_enabled() -> bool {
    mcp::load_mcp_servers_config_raw()
        .map(|config| config.auto_reauthorize)
        .unwrap_or(true)
}

/// Start re-authorizing `server` in the background when its login expired and
/// automatic re-authorization is on, without waiting for it. For a connect
/// that found the login expired: the server's tools appear on the next run,
/// once the new token is stored.
pub fn reauthorize_in_background(server: &str, config: &McpServerConfig) {
    if mcp::has_mcp_oauth_tokens(server) && auto_reauthorize_enabled() {
        tracing::info!("MCP server '{server}' login expired; re-authorizing");
        drop(relogin(server, config));
    }
}

/// Log the HTTP MCP server `server`, configured as `config`, in through the
/// browser, joining a login already running for it.
pub async fn login_shared(server: &str, config: &McpServerConfig) -> Result<()> {
    relogin(server, config).await.map_err(anyhow::Error::msg)
}

type Login = Shared<BoxFuture<'static, Result<(), String>>>;

/// Logins in progress per server, so concurrent requests share one browser
/// round trip.
static LOGINS: LazyLock<Mutex<HashMap<String, Login>>> = LazyLock::new(Default::default);

/// Join or start the browser login for `server`. It runs as its own task: a
/// caller that gives up (a cancelled tool call) does not abandon a login the
/// user may be completing in the browser.
fn relogin(server: &str, config: &McpServerConfig) -> Login {
    let mut logins = LOGINS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(login) = logins.get(server) {
        return login.clone();
    }
    let name = server.to_string();
    let config = config.clone();
    let task = tokio::spawn(async move {
        let result = login_configured_server(&name, &config)
            .await
            .map_err(|e| format!("{e:#}"));
        LOGINS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&name);
        result
    });
    let login = async move {
        task.await
            .unwrap_or_else(|e| Err(format!("the login task failed: {e}")))
    }
    .boxed()
    .shared();
    logins.insert(server.to_string(), login.clone());
    login
}

/// Read the redirect request off `stream` and return its decoded query
/// parameters.
async fn read_callback_query(
    stream: &mut tokio::net::TcpStream,
) -> Result<std::collections::HashMap<String, String>> {
    use tokio::io::AsyncReadExt;

    let mut buf = vec![0u8; 8192];
    let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buf))
        .await
        .context("timeout reading the OAuth callback request")?
        .context("reading the OAuth callback request")?;
    let request = String::from_utf8_lossy(&buf[..n]);

    let request_line = request.lines().next().unwrap_or("");
    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let query = path.split('?').nth(1).unwrap_or("");

    Ok(query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next()?;
            let value = parts.next().unwrap_or("");
            Some((
                urlencoding::decode(key).ok()?.into_owned(),
                urlencoding::decode(value).ok()?.into_owned(),
            ))
        })
        .collect())
}

/// Write a minimal HTML response and close the connection.
async fn respond(stream: &mut tokio::net::TcpStream, status: &str, title: &str, body: &str) {
    use tokio::io::AsyncWriteExt;

    let html = format!(
        "<html><body style=\"font-family: system-ui, -apple-system, sans-serif; \
         display: flex; justify-content: center; align-items: center; height: 100vh; \
         margin: 0; background: #f5f5f5;\"><div style=\"text-align: center; padding: 2rem;\">\
         <h1 style=\"color: #333;\">{title}</h1><p style=\"color: #666;\">{body}</p></div></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With the setting off, or without a previous login, nothing opens a
    /// browser: the call fails with a reason the user can act on.
    #[tokio::test]
    async fn reauthorization_declines_when_off_or_never_logged_in() {
        use mcp_client::Reauthorizer;
        let config = tempfile::tempdir().unwrap();
        temp_env::async_with_vars(
            [("CODE_ASSISTANT_CONFIG_DIR", Some(config.path()))],
            async {
                let reauthorizer = AutoReauthorizer::new(McpServerConfig {
                    transport: mcp::McpTransport::http("https://mcp.example.com/mcp"),
                    enabled: true,
                    off_in_new_sessions: false,
                    enabled_tools: None,
                    disabled_tools: Vec::new(),
                });
                let error = reauthorizer.reauthorize("remote").await.unwrap_err();
                assert!(
                    error.to_string().contains("requires authorization"),
                    "{error}"
                );

                std::fs::create_dir_all(mcp::mcp_oauth_dir()).unwrap();
                std::fs::write(mcp::mcp_oauth_token_path("remote"), "{}").unwrap();
                let servers = mcp::McpServersConfig {
                    auto_reauthorize: false,
                    ..Default::default()
                };
                mcp::save_mcp_servers_config(&servers).unwrap();
                assert!(!auto_reauthorize_enabled());
                let error = reauthorizer.reauthorize("remote").await.unwrap_err();
                assert!(error.to_string().contains("is off"), "{error}");
            },
        )
        .await;
    }

    #[tokio::test]
    async fn bind_yields_a_loopback_redirect_uri() {
        let authorizer = LoopbackAuthorizer::bind().await.unwrap();
        let uri = authorizer.redirect_uri();
        assert!(
            uri.starts_with("http://127.0.0.1:") && uri.ends_with("/callback"),
            "unexpected redirect uri: {uri}"
        );
    }
}
