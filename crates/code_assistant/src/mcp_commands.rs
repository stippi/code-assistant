//! CLI commands for authenticating HTTP MCP servers that require OAuth
//! (MCP's authorization spec — e.g. servers that answer `initialize` with
//! `401 Auth required`).
//!
//! `mcp-login <server>` runs the browser OAuth flow and stores the resulting
//! tokens under `<config_dir>/mcp-oauth/<server>.json`; subsequent agent runs
//! reuse them silently. This mirrors what Cline's "Authenticate" button does.

use anyhow::{Context, Result};
use code_assistant_core::tools::mcp::{
    self, AuthorizationOutcome, OAuthAuthorizer, authenticate_mcp_server,
};
use std::sync::Mutex;
use std::time::Duration;
use tokio::net::TcpListener;

/// An [`OAuthAuthorizer`] that runs a one-shot loopback HTTP server for the
/// OAuth redirect (RFC 8252 native-app pattern) and opens the user's browser
/// to the authorization URL.
struct LoopbackAuthorizer {
    redirect_uri: String,
    /// Bound at construction so [`redirect_uri`] reflects the real port;
    /// consumed by the single `authorize` call.
    listener: Mutex<Option<TcpListener>>,
}

impl LoopbackAuthorizer {
    /// Bind an ephemeral loopback port for the redirect callback.
    async fn bind() -> Result<Self> {
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

        println!("Opening your browser to authorize with the MCP server.");
        println!("If it doesn't open, visit this URL manually:");
        println!();
        println!("  {authorization_url}");
        println!();
        println!("Waiting for the authorization callback (up to 5 minutes)...");

        if let Err(e) = open::that(&authorization_url) {
            eprintln!("Could not open the browser automatically: {e}");
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
                "You can close this window and return to the terminal.",
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
            "You can close this window and return to the terminal.",
        )
        .await;

        Ok(AuthorizationOutcome {
            code,
            state,
            issuer,
        })
    }
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

/// Run the OAuth browser login for the configured HTTP MCP server `server`.
pub async fn run_mcp_login(server: &str) -> Result<()> {
    println!("Starting OAuth login for MCP server '{server}'...");
    let authorizer = LoopbackAuthorizer::bind().await?;
    authenticate_mcp_server(server, &authorizer).await?;
    println!();
    println!("Login successful. Tokens stored for MCP server '{server}'.");
    println!("The next agent run will use them automatically.");
    Ok(())
}

/// Remove any stored OAuth tokens for the MCP server `server`.
pub fn run_mcp_logout(server: &str) -> Result<()> {
    if mcp::forget_mcp_oauth_tokens(server)? {
        println!("Logged out. Removed stored OAuth tokens for MCP server '{server}'.");
    } else {
        println!("No stored OAuth tokens for MCP server '{server}'.");
    }
    Ok(())
}
