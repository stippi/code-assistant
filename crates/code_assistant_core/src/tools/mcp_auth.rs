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
//! expired, if `auto_reauthorize` is on in `mcp-servers.json`. With a
//! [`LoginConsent`] (the GPUI frontend's [`LoginRequests`]), the user is asked
//! first, so the switch to the browser never comes out of the blue.

use super::mcp::{self, AuthorizationOutcome, McpServerConfig, OAuthAuthorizer};
use anyhow::{Context, Result};
use futures::FutureExt as _;
use futures::future::{BoxFuture, Shared};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;

use crate::session::event_stream::EventStream;
use crate::ui::ui_events::UiEvent;

/// Asks the user before an automatic renewal opens the browser.
#[async_trait::async_trait]
pub trait LoginConsent: Send + Sync {
    /// Whether the login of `server` may start now.
    async fn confirm(&self, server: &str) -> bool;
}

/// How long a renewal waits for the user's answer before giving up.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);

/// A [`LoginConsent`] that asks through the app-scoped
/// [`UiEvent::McpLoginsRequested`]; a frontend answers with
/// [`LoginRequests::answer`] (via `SessionService::answer_mcp_login`).
#[derive(Default)]
pub struct LoginRequests {
    /// Waiting renewals by server name, in the order they asked.
    pending: Mutex<Vec<(String, Vec<tokio::sync::oneshot::Sender<bool>>)>>,
    events: std::sync::OnceLock<EventStream>,
}

impl LoginRequests {
    /// Publish the requests to `events`.
    pub fn set_event_stream(&self, events: EventStream) {
        let _ = self.events.set(events);
    }

    /// The servers waiting for an answer, in the order they asked.
    pub fn servers(&self) -> Vec<String> {
        self.locked().iter().map(|(name, _)| name.clone()).collect()
    }

    /// Answer the request for `server`: `ok` starts its login. Without a
    /// request for it, nothing happens.
    pub fn answer(&self, server: &str, ok: bool) {
        let waiters = {
            let mut pending = self.locked();
            let Some(index) = pending.iter().position(|(name, _)| name == server) else {
                return;
            };
            pending.remove(index).1
        };
        for waiter in waiters {
            let _ = waiter.send(ok);
        }
        self.publish();
    }

    fn locked(
        &self,
    ) -> std::sync::MutexGuard<'_, Vec<(String, Vec<tokio::sync::oneshot::Sender<bool>>)>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn publish(&self) {
        if let Some(events) = self.events.get() {
            events.publish_app(UiEvent::McpLoginsRequested {
                servers: self.servers(),
            });
        }
    }
}

#[async_trait::async_trait]
impl LoginConsent for LoginRequests {
    async fn confirm(&self, server: &str) -> bool {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        {
            let mut pending = self.locked();
            match pending.iter_mut().find(|(name, _)| name == server) {
                Some((_, waiters)) => waiters.push(sender),
                None => pending.push((server.to_string(), vec![sender])),
            }
        }
        self.publish();
        match tokio::time::timeout(CONSENT_TIMEOUT, receiver).await {
            Ok(answer) => answer.unwrap_or(false),
            Err(_) => {
                // Nobody answered: withdraw the question.
                self.answer(server, false);
                false
            }
        }
    }
}

/// An [`OAuthAuthorizer`] that runs a one-shot loopback HTTP server for the
/// OAuth redirect and opens the user's browser to the authorization URL.
pub struct LoopbackAuthorizer {
    redirect_uri: String,
    /// Bound at construction so [`Self::redirect_uri`] reflects the real port;
    /// consumed by the single `authorize` call.
    listener: Mutex<Option<TcpListener>>,
    /// Where `authorize` leaves the authorization URL, so the browser can be
    /// opened at it again while this login waits.
    url: UrlSlot,
}

/// The authorization URL of a login, once it is known.
type UrlSlot = Arc<Mutex<Option<String>>>;

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
            url: UrlSlot::default(),
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
        *self.url.lock().unwrap_or_else(|e| e.into_inner()) = Some(authorization_url.clone());
        open_browser(&authorization_url);

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

fn open_browser(url: &str) {
    if let Err(e) = open::that(url) {
        tracing::warn!("Could not open the browser automatically: {e}");
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
    login_reporting_url(name, server, UrlSlot::default()).await
}

/// [`login_configured_server`], leaving the authorization URL in `url`.
async fn login_reporting_url(name: &str, server: &McpServerConfig, url: UrlSlot) -> Result<()> {
    let mut authorizer = LoopbackAuthorizer::bind().await?;
    authorizer.url = url;
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
    /// Asked before the browser opens; `None` opens it at once.
    consent: Option<Arc<dyn LoginConsent>>,
}

impl AutoReauthorizer {
    pub fn new(server: McpServerConfig, consent: Option<Arc<dyn LoginConsent>>) -> Self {
        Self { server, consent }
    }
}

#[async_trait::async_trait]
impl mcp_client::Reauthorizer for AutoReauthorizer {
    async fn reauthorize(&self, server: &str) -> Result<()> {
        if !mcp::has_mcp_oauth_tokens(server, &self.server) {
            anyhow::bail!("MCP server '{server}' requires authorization");
        }
        if !auto_reauthorize_enabled() {
            anyhow::bail!(
                "the login of MCP server '{server}' expired and automatic re-authorization \
                 is off; log in again in the MCP server settings (or run \
                 `code-assistant mcp-login {server}`)"
            );
        }
        relogin(server, &self.server, self.consent.clone())
            .0
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
/// automatic re-authorization is on, without waiting for it — after asking
/// `consent`, when given. For a connect that found the login expired: the
/// server's tools appear on the next run, once the new token is stored.
pub fn reauthorize_in_background(
    server: &str,
    config: &McpServerConfig,
    consent: Option<Arc<dyn LoginConsent>>,
) {
    if mcp::has_mcp_oauth_tokens(server, config) && auto_reauthorize_enabled() {
        tracing::info!("MCP server '{server}' login expired; re-authorizing");
        drop(relogin(server, config, consent).0);
    }
}

/// Log the HTTP MCP server `server`, configured as `config`, in through the
/// browser — asked for by the user. Joins a login already running for it and
/// opens the browser at its page again: the user may have closed it, and the
/// waiting login still takes the callback.
pub async fn login_shared(server: &str, config: &McpServerConfig) -> Result<()> {
    let (login, joined) = relogin(server, config, None);
    let url = joined.and_then(|url| url.lock().unwrap_or_else(|e| e.into_inner()).clone());
    if let Some(url) = url {
        tracing::info!("MCP server '{server}' login is waiting; opening its page again");
        open_browser(&url);
    }
    login.await.map_err(anyhow::Error::msg)
}

type Login = Shared<BoxFuture<'static, Result<(), String>>>;

/// Logins in progress by token file (server name and URL, see
/// [`mcp::mcp_oauth_token_path`]), so concurrent requests share one browser
/// round trip, with where each one's authorization URL appears.
static LOGINS: LazyLock<Mutex<HashMap<PathBuf, (Login, UrlSlot)>>> =
    LazyLock::new(Default::default);

/// Join or start the browser login for `server`. It runs as its own task: a
/// caller that gives up (a cancelled tool call) does not abandon a login the
/// user may be completing in the browser. A new login asks `consent` first,
/// when given; one the user postpones fails. Returns the login and, when it
/// joined one already running, that login's URL slot.
fn relogin(
    server: &str,
    config: &McpServerConfig,
    consent: Option<Arc<dyn LoginConsent>>,
) -> (Login, Option<UrlSlot>) {
    let key = mcp::mcp_oauth_token_path(server, config);
    let mut logins = LOGINS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((login, url)) = logins.get(&key) {
        return (login.clone(), Some(url.clone()));
    }
    let name = server.to_string();
    let config = config.clone();
    let url = UrlSlot::default();
    let task_url = url.clone();
    let task_key = key.clone();
    let task = tokio::spawn(async move {
        let confirmed = match &consent {
            Some(consent) => consent.confirm(&name).await,
            None => true,
        };
        let result = if confirmed {
            login_reporting_url(&name, &config, task_url)
                .await
                .map_err(|e| format!("{e:#}"))
        } else {
            Err(format!(
                "the login of MCP server '{name}' expired and logging in again was \
                 postponed; log in from the Tools menu"
            ))
        };
        LOGINS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&task_key);
        result
    });
    let login = async move {
        task.await
            .unwrap_or_else(|e| Err(format!("the login task failed: {e}")))
    }
    .boxed()
    .shared();
    logins.insert(key, (login.clone(), url));
    (login, None)
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
                let reauthorizer = AutoReauthorizer::new(
                    McpServerConfig {
                        transport: mcp::McpTransport::http("https://mcp.example.com/mcp"),
                        enabled: true,
                        off_in_new_sessions: false,
                        enabled_tools: None,
                        disabled_tools: Vec::new(),
                    },
                    None,
                );
                let error = reauthorizer.reauthorize("remote").await.unwrap_err();
                assert!(
                    error.to_string().contains("requires authorization"),
                    "{error}"
                );

                std::fs::create_dir_all(mcp::mcp_oauth_dir()).unwrap();
                std::fs::write(
                    mcp::mcp_oauth_token_path("remote", &reauthorizer.server),
                    "{}",
                )
                .unwrap();
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

    fn login_requests_published(
        subscription: &mut crate::session::event_stream::Subscription,
    ) -> Vec<Vec<String>> {
        use futures::FutureExt;
        let mut seen = Vec::new();
        while let Some(Ok(event)) = subscription.recv().now_or_never() {
            if let crate::session::event_stream::EventPayload::Ui(UiEvent::McpLoginsRequested {
                servers,
            }) = event.payload
            {
                assert!(event.session_id.is_none(), "requests are app-scoped");
                seen.push(servers);
            }
        }
        seen
    }

    /// A renewal asks through the event stream and waits; requests for one
    /// server share one question, and the answer reaches all of them.
    #[tokio::test]
    async fn login_requests_ask_once_per_server_and_pass_the_answer_on() {
        let requests = LoginRequests::default();
        let events = EventStream::new();
        let mut subscription = events.subscribe();
        requests.set_event_stream(events);

        // Polled once each, so all three have asked before anyone answers.
        let mut first = requests.confirm("jira");
        let mut second = requests.confirm("jira");
        let mut other = requests.confirm("wiki");
        assert!(futures::poll!(&mut first).is_pending());
        assert!(futures::poll!(&mut second).is_pending());
        assert!(futures::poll!(&mut other).is_pending());
        assert_eq!(requests.servers(), vec!["jira", "wiki"]);
        assert_eq!(
            login_requests_published(&mut subscription).last(),
            Some(&vec!["jira".to_string(), "wiki".to_string()])
        );

        requests.answer("jira", true);
        assert!(first.await);
        assert!(second.await);
        assert_eq!(
            login_requests_published(&mut subscription).last(),
            Some(&vec!["wiki".to_string()])
        );

        requests.answer("wiki", false);
        assert!(!other.await);
        assert!(requests.servers().is_empty());
        requests.answer("wiki", true); // nothing waits: no effect
    }

    struct Decline;

    #[async_trait::async_trait]
    impl LoginConsent for Decline {
        async fn confirm(&self, _server: &str) -> bool {
            false
        }
    }

    /// A postponed login fails the call with a hint, and the browser never
    /// opens (the authorizer is not even bound).
    #[tokio::test]
    async fn a_postponed_login_fails_without_opening_the_browser() {
        use mcp_client::Reauthorizer;
        let config = tempfile::tempdir().unwrap();
        temp_env::async_with_vars(
            [("CODE_ASSISTANT_CONFIG_DIR", Some(config.path()))],
            async {
                let server = McpServerConfig {
                    transport: mcp::McpTransport::http("https://postponed.example.com/mcp"),
                    enabled: true,
                    off_in_new_sessions: false,
                    enabled_tools: None,
                    disabled_tools: Vec::new(),
                };
                std::fs::create_dir_all(mcp::mcp_oauth_dir()).unwrap();
                std::fs::write(mcp::mcp_oauth_token_path("postponed", &server), "{}").unwrap();
                let reauthorizer = AutoReauthorizer::new(server, Some(Arc::new(Decline)));
                let error = reauthorizer.reauthorize("postponed").await.unwrap_err();
                assert!(error.to_string().contains("postponed"), "{error}");
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
