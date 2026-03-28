//! OpenAI Codex authentication module.
//!
//! Implements the OAuth2 PKCE authorization code flow used by OpenAI Codex CLI
//! to authenticate with a ChatGPT subscription. This allows users to use their
//! existing ChatGPT Plus/Pro/Team subscription instead of a separate API key.
//!
//! # Flow
//!
//! 1. Generate PKCE verifier + challenge
//! 2. Start a local HTTP server on port 1455
//! 3. Open the browser to `auth.openai.com/oauth/authorize`
//! 4. User authenticates, browser redirects to localhost with auth code
//! 5. Exchange auth code + PKCE verifier for tokens (id_token, access_token, refresh_token)
//! 6. Extract ChatGPT account ID from id_token JWT claims
//! 7. Persist tokens to disk (with 0o600 permissions on Unix)
//! 8. Use access_token as Bearer token + ChatGPT-Account-ID header for API requests
//!
//! The API base URL for ChatGPT-authenticated requests is
//! `https://chatgpt.com/backend-api/codex` instead of `https://api.openai.com/v1`.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use tracing::{debug, info, warn};

// Re-export for consumers
pub use crate::openai_responses::{AuthProvider, RequestCustomizer};

/// OpenAI OAuth client ID (shared with Codex CLI).
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Default OAuth issuer.
pub const ISSUER: &str = "https://auth.openai.com";

/// Local redirect port. Must be 1455 to match the redirect URI registered
/// for the OAuth client ID at auth.openai.com.
const REDIRECT_PORT: u16 = 1455;

/// Token refresh URL.
const REFRESH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

/// ChatGPT backend API base URL for Codex-authenticated requests.
pub const CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

/// How often to refresh tokens (in days). Codex CLI uses 8 days.
const TOKEN_REFRESH_INTERVAL_DAYS: i64 = 8;

/// OAuth scope requested during authorization.
const OAUTH_SCOPE: &str = "openid profile email offline_access";

// ---------------------------------------------------------------------------
// PKCE
// ---------------------------------------------------------------------------

/// Generate a PKCE code verifier and challenge (S256 method).
pub fn generate_pkce_pair() -> (String, String) {
    let mut verifier_bytes = [0u8; 64];
    for byte in &mut verifier_bytes {
        *byte = rand::random();
    }
    let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);

    let challenge_hash = sha256(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(challenge_hash);

    (verifier, challenge)
}

/// SHA-256 hash implementation (pure Rust, no extra dependencies).
fn sha256(data: &[u8]) -> [u8; 32] {
    // SHA-256 constants
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    // Pre-processing: padding
    let bit_len = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    // Process each 512-bit block
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut result = [0u8; 32];
    for (i, &val) in h.iter().enumerate() {
        result[i * 4..i * 4 + 4].copy_from_slice(&val.to_be_bytes());
    }
    result
}

// ---------------------------------------------------------------------------
// Token types and persistence
// ---------------------------------------------------------------------------

/// OAuth tokens obtained from the authorization flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexTokens {
    /// OAuth ID token (JWT with user claims).
    pub id_token: String,
    /// OAuth access token (used as Bearer token for API requests).
    pub access_token: String,
    /// OAuth refresh token (for obtaining new access tokens).
    pub refresh_token: String,
    /// ChatGPT account ID extracted from id_token claims.
    pub account_id: Option<String>,
}

/// Persisted auth state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexAuthState {
    /// The OAuth tokens.
    pub tokens: CodexTokens,
    /// When the tokens were last refreshed.
    pub last_refresh: DateTime<Utc>,
}

impl CodexAuthState {
    /// Check if tokens need refreshing.
    pub fn needs_refresh(&self) -> bool {
        let elapsed = Utc::now() - self.last_refresh;
        elapsed.num_days() >= TOKEN_REFRESH_INTERVAL_DAYS
    }
}

/// Extract the ChatGPT account ID from the id_token JWT claims.
///
/// The id_token contains a claim at `https://api.openai.com/auth` with
/// a `chatgpt_account_id` field.
pub fn extract_account_id(id_token: &str) -> Option<String> {
    // JWT is base64url(header).base64url(payload).base64url(signature)
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }

    let payload = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&payload).ok()?;

    claims
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// Token storage
// ---------------------------------------------------------------------------

/// Storage path for Codex auth tokens.
///
/// Default: `~/.config/code-assistant/codex_auth.json`
/// (falls back to `~/.config/codex_auth.json` if `dirs::config_dir()` is unavailable).
fn default_auth_path() -> PathBuf {
    let config_dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    config_dir.join("code-assistant").join("codex_auth.json")
}

/// Get the default auth path (public, for use by CLI commands and factory).
pub fn default_codex_auth_path() -> PathBuf {
    default_auth_path()
}

/// Save auth state to disk.
pub fn save_auth_state(state: &CodexAuthState, path: Option<&Path>) -> Result<()> {
    let path = path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(default_auth_path);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(state)?;
    std::fs::write(&path, &json)?;

    // Set file permissions to 0600 on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

    info!("Saved Codex auth state to {}", path.display());
    Ok(())
}

/// Load auth state from disk.
pub fn load_auth_state(path: Option<&Path>) -> Result<Option<CodexAuthState>> {
    let path = path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(default_auth_path);

    if !path.exists() {
        return Ok(None);
    }

    let json = std::fs::read_to_string(&path)?;
    let state: CodexAuthState = serde_json::from_str(&json)?;
    Ok(Some(state))
}

/// Delete auth state from disk.
pub fn delete_auth_state(path: Option<&Path>) -> Result<()> {
    let path = path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(default_auth_path);

    if path.exists() {
        std::fs::remove_file(&path)?;
        info!("Deleted Codex auth state from {}", path.display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// OAuth login flow
// ---------------------------------------------------------------------------

/// Result of a successful login.
#[derive(Debug, Clone)]
pub struct LoginResult {
    pub tokens: CodexTokens,
    pub auth_state: CodexAuthState,
}

/// Run the OAuth PKCE browser login flow.
///
/// 1. Generates PKCE pair
/// 2. Starts local HTTP server on `REDIRECT_PORT`
/// 3. Returns the authorization URL (caller should open in browser)
/// 4. Waits for the callback
/// 5. Exchanges code for tokens
/// 6. Returns the login result
pub async fn start_login_flow(
    auth_state_path: Option<&Path>,
) -> Result<(String, tokio::sync::oneshot::Receiver<Result<LoginResult>>)> {
    let (verifier, challenge) = generate_pkce_pair();
    let state_param = generate_random_state();

    let redirect_uri = format!("http://localhost:{}/auth/callback", REDIRECT_PORT);

    let authorize_url = build_authorize_url(&challenge, &state_param, &redirect_uri);

    let (tx, rx) = tokio::sync::oneshot::channel();

    let verifier_clone = verifier.clone();
    let state_param_clone = state_param.clone();
    let redirect_uri_clone = redirect_uri.clone();
    let auth_path = auth_state_path.map(|p| p.to_path_buf());

    // Spawn the local server to wait for the callback
    tokio::spawn(async move {
        let result = run_callback_server(
            verifier_clone,
            state_param_clone,
            redirect_uri_clone,
            auth_path.as_deref(),
        )
        .await;
        let _ = tx.send(result);
    });

    Ok((authorize_url, rx))
}

/// Build the OAuth authorization URL.
///
/// The parameters must match what the Codex CLI sends -- in particular
/// `codex_cli_simplified_flow=true` is required for the server to accept
/// the request with this client ID.
fn build_authorize_url(challenge: &str, state: &str, redirect_uri: &str) -> String {
    format!(
        "{}/oauth/authorize?\
         response_type=code\
         &client_id={}\
         &redirect_uri={}\
         &scope={}\
         &code_challenge={}\
         &code_challenge_method=S256\
         &id_token_add_organizations=true\
         &codex_cli_simplified_flow=true\
         &state={}\
         &originator=codex_cli",
        ISSUER,
        urlencoding::encode(CLIENT_ID),
        urlencoding::encode(redirect_uri),
        urlencoding::encode(OAUTH_SCOPE),
        urlencoding::encode(challenge),
        urlencoding::encode(state),
    )
}

/// Generate a random state parameter for CSRF protection.
fn generate_random_state() -> String {
    let mut bytes = [0u8; 32];
    for byte in &mut bytes {
        *byte = rand::random();
    }
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Run the local callback server, wait for the OAuth redirect, and exchange the code.
async fn run_callback_server(
    verifier: String,
    expected_state: String,
    redirect_uri: String,
    auth_state_path: Option<&Path>,
) -> Result<LoginResult> {
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", REDIRECT_PORT))
        .await
        .context("Failed to bind callback server")?;

    info!("OAuth callback server listening on port {}", REDIRECT_PORT);

    // Wait for a connection with a 5-minute timeout
    let (mut stream, _) =
        tokio::time::timeout(std::time::Duration::from_secs(300), listener.accept())
            .await
            .context("Login timed out after 5 minutes")?
            .context("Failed to accept connection")?;

    // Read the HTTP request (async)
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(std::time::Duration::from_secs(10), stream.read(&mut buf))
        .await
        .context("Timeout reading HTTP request")?
        .context("Failed to read HTTP request")?;
    let request = String::from_utf8_lossy(&buf[..n]);

    // Parse the request line to get the path + query
    let request_line = request.lines().next().unwrap_or("");
    let path = request_line.split_whitespace().nth(1).unwrap_or("/");

    // Extract query parameters
    let query_string = path.split('?').nth(1).unwrap_or("");
    let params: std::collections::HashMap<String, String> = query_string
        .split('&')
        .filter_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next()?;
            let value = parts.next().unwrap_or("");
            Some((
                urlencoding::decode(key).ok()?.to_string(),
                urlencoding::decode(value).ok()?.to_string(),
            ))
        })
        .collect();

    // Validate state
    let state = params.get("state").cloned().unwrap_or_default();
    if state != expected_state {
        // Send error response
        let response = "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\n\r\n<html><body><h1>Error</h1><p>State mismatch. Please try again.</p></body></html>";
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
        bail!("OAuth state mismatch");
    }

    // Check for error
    if let Some(error) = params.get("error") {
        let desc = params.get("error_description").cloned().unwrap_or_default();
        let response = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\n\r\n<html><body><h1>Login Error</h1><p>{}: {}</p></body></html>",
            error, desc
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
        bail!("OAuth error: {} - {}", error, desc);
    }

    // Extract auth code
    let code = params
        .get("code")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("No authorization code in callback"))?;

    // Send success response
    let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n\
        <html><body style=\"font-family: system-ui, -apple-system, sans-serif; display: flex; justify-content: center; align-items: center; height: 100vh; margin: 0; background: #f5f5f5;\">\
        <div style=\"text-align: center; padding: 2rem;\">\
        <h1 style=\"color: #333;\">Login Successful</h1>\
        <p style=\"color: #666;\">You can close this window and return to the app.</p>\
        </div></body></html>";
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;

    // Exchange code for tokens
    let tokens = exchange_code_for_tokens(&code, &verifier, &redirect_uri).await?;

    let auth_state = CodexAuthState {
        tokens: tokens.clone(),
        last_refresh: Utc::now(),
    };

    // Save to disk
    save_auth_state(&auth_state, auth_state_path)?;

    Ok(LoginResult { tokens, auth_state })
}

/// Exchange an authorization code for tokens.
async fn exchange_code_for_tokens(
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<CodexTokens> {
    let client = reqwest::Client::new();

    let body = format!(
        "grant_type=authorization_code\
         &code={}\
         &redirect_uri={}\
         &client_id={}\
         &code_verifier={}",
        urlencoding::encode(code),
        urlencoding::encode(redirect_uri),
        urlencoding::encode(CLIENT_ID),
        urlencoding::encode(verifier),
    );

    debug!("Exchanging auth code for tokens");

    let response = client
        .post(format!("{}/oauth/token", ISSUER))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .context("Failed to send token exchange request")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("Token exchange failed with status {}: {}", status, body);
    }

    #[derive(Deserialize)]
    struct TokenResponse {
        id_token: String,
        access_token: String,
        refresh_token: String,
    }

    let token_response: TokenResponse = response
        .json()
        .await
        .context("Failed to parse token response")?;

    let account_id = extract_account_id(&token_response.id_token);
    if account_id.is_none() {
        warn!("Could not extract ChatGPT account ID from id_token");
    }

    Ok(CodexTokens {
        id_token: token_response.id_token,
        access_token: token_response.access_token,
        refresh_token: token_response.refresh_token,
        account_id,
    })
}

/// Refresh the access token using the refresh token.
pub async fn refresh_tokens(auth_state: &CodexAuthState) -> Result<CodexAuthState> {
    let client = reqwest::Client::new();

    let body = serde_json::json!({
        "client_id": CLIENT_ID,
        "grant_type": "refresh_token",
        "refresh_token": auth_state.tokens.refresh_token,
        "scope": "openid profile email",
    });

    debug!("Refreshing Codex auth tokens");

    let response = client
        .post(REFRESH_TOKEN_URL)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .context("Failed to send token refresh request")?;

    if !response.status().is_success() {
        let status = response.status();
        let body_text = response.text().await.unwrap_or_default();

        // Parse specific error codes
        if status.as_u16() == 401 {
            if let Ok(error_body) = serde_json::from_str::<serde_json::Value>(&body_text) {
                if let Some(error_code) = error_body.get("error_code").and_then(|v| v.as_str()) {
                    match error_code {
                        "refresh_token_expired" => {
                            bail!("Refresh token expired. Please log in again.")
                        }
                        "refresh_token_reused" => {
                            bail!("Refresh token was already used. Please log in again.")
                        }
                        "refresh_token_invalidated" => {
                            bail!("Refresh token was revoked. Please log in again.")
                        }
                        _ => {}
                    }
                }
            }
        }

        bail!("Token refresh failed with status {}: {}", status, body_text);
    }

    #[derive(Deserialize)]
    struct RefreshResponse {
        id_token: String,
        access_token: String,
        refresh_token: String,
    }

    let refresh_response: RefreshResponse = response
        .json()
        .await
        .context("Failed to parse refresh response")?;

    let account_id = extract_account_id(&refresh_response.id_token)
        .or_else(|| auth_state.tokens.account_id.clone());

    Ok(CodexAuthState {
        tokens: CodexTokens {
            id_token: refresh_response.id_token,
            access_token: refresh_response.access_token,
            refresh_token: refresh_response.refresh_token,
            account_id,
        },
        last_refresh: Utc::now(),
    })
}

// ---------------------------------------------------------------------------
// AuthProvider / RequestCustomizer for OpenAI Responses API with ChatGPT auth
// ---------------------------------------------------------------------------

/// Manages Codex auth tokens and provides them to the OpenAI Responses client.
///
/// Handles automatic token refresh when needed.
pub struct CodexAuthProvider {
    state: Arc<RwLock<CodexAuthState>>,
    auth_state_path: Option<PathBuf>,
}

impl CodexAuthProvider {
    /// Create a new Codex auth provider from persisted state.
    pub fn new(state: CodexAuthState, auth_state_path: Option<PathBuf>) -> Self {
        Self {
            state: Arc::new(RwLock::new(state)),
            auth_state_path,
        }
    }

    /// Get the current auth state.
    pub fn get_state(&self) -> CodexAuthState {
        self.state.read().unwrap().clone()
    }

    /// Try to refresh tokens if needed. Returns true if refresh was performed.
    async fn ensure_fresh_tokens(&self) -> Result<()> {
        let needs_refresh = {
            let state = self.state.read().unwrap();
            state.needs_refresh()
        };

        if needs_refresh {
            let current_state = self.state.read().unwrap().clone();
            match refresh_tokens(&current_state).await {
                Ok(new_state) => {
                    save_auth_state(&new_state, self.auth_state_path.as_deref())?;
                    let mut state = self.state.write().unwrap();
                    *state = new_state;
                    info!("Successfully refreshed Codex auth tokens");
                }
                Err(e) => {
                    warn!("Failed to refresh Codex auth tokens: {}", e);
                    return Err(e);
                }
            }
        }

        Ok(())
    }
}

#[async_trait]
impl AuthProvider for CodexAuthProvider {
    async fn get_auth_headers(&self) -> Result<Vec<(String, String)>> {
        // Ensure tokens are fresh before returning headers
        self.ensure_fresh_tokens().await?;

        let state = self.state.read().unwrap();
        let mut headers = vec![(
            "Authorization".to_string(),
            format!("Bearer {}", state.tokens.access_token),
        )];

        if let Some(ref account_id) = state.tokens.account_id {
            headers.push(("ChatGPT-Account-ID".to_string(), account_id.clone()));
        }

        Ok(headers)
    }
}

/// Request customizer for ChatGPT-authenticated Codex requests.
///
/// Uses the ChatGPT backend URL (`chatgpt.com/backend-api/codex`) and
/// adjusts headers accordingly.
pub struct CodexRequestCustomizer;

impl RequestCustomizer for CodexRequestCustomizer {
    fn customize_request(&self, _request: &mut serde_json::Value) -> Result<()> {
        Ok(())
    }

    fn get_additional_headers(&self) -> Vec<(String, String)> {
        // No beta header needed for ChatGPT backend
        vec![]
    }

    fn customize_url(&self, base_url: &str, _streaming: bool) -> String {
        format!("{base_url}/responses")
    }
}

// ---------------------------------------------------------------------------
// Auth status for the frontend
// ---------------------------------------------------------------------------

/// Status of Codex authentication, serializable for the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexAuthStatus {
    /// Whether the user is authenticated via Codex OAuth.
    pub authenticated: bool,
    /// The email from the id_token, if available.
    pub email: Option<String>,
    /// The ChatGPT plan type, if available.
    pub plan_type: Option<String>,
    /// Whether tokens need refreshing.
    pub needs_refresh: bool,
}

/// Get the current Codex auth status from persisted state.
pub fn get_auth_status(auth_state_path: Option<&Path>) -> CodexAuthStatus {
    match load_auth_state(auth_state_path) {
        Ok(Some(state)) => {
            let (email, plan_type) = extract_user_info(&state.tokens.id_token);
            CodexAuthStatus {
                authenticated: true,
                email,
                plan_type,
                needs_refresh: state.needs_refresh(),
            }
        }
        _ => CodexAuthStatus {
            authenticated: false,
            email: None,
            plan_type: None,
            needs_refresh: false,
        },
    }
}

/// Extract email and plan type from the id_token.
fn extract_user_info(id_token: &str) -> (Option<String>, Option<String>) {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 {
        return (None, None);
    }

    let payload = match URL_SAFE_NO_PAD.decode(parts[1]) {
        Ok(p) => p,
        Err(_) => return (None, None),
    };

    let claims: serde_json::Value = match serde_json::from_slice(&payload) {
        Ok(c) => c,
        Err(_) => return (None, None),
    };

    let email = claims
        .get("email")
        .and_then(|v| v.as_str())
        .map(String::from);

    let plan_type = claims
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_plan_type"))
        .and_then(|v| v.as_str())
        .map(String::from);

    (email, plan_type)
}

// ---------------------------------------------------------------------------
// Convenience: create an OpenAI Responses client with Codex auth
// ---------------------------------------------------------------------------

/// Create an `OpenAIResponsesClient` configured for ChatGPT-authenticated requests.
pub fn create_codex_responses_client(
    auth_state: CodexAuthState,
    model: String,
    auth_state_path: Option<PathBuf>,
) -> crate::openai_responses::OpenAIResponsesClient {
    let auth_provider = Box::new(CodexAuthProvider::new(auth_state, auth_state_path));
    let request_customizer = Box::new(CodexRequestCustomizer);

    crate::openai_responses::OpenAIResponsesClient::with_customization(
        model,
        CHATGPT_BASE_URL.to_string(),
        auth_provider,
        request_customizer,
    )
}

/// Create an `OpenAIResponsesWsClient` configured for ChatGPT-authenticated
/// requests over WebSocket.
///
/// This uses the same Codex OAuth tokens but communicates via a persistent
/// WebSocket connection instead of HTTP/SSE, matching the Codex CLI transport.
pub fn create_codex_responses_ws_client(
    auth_state: CodexAuthState,
    model: String,
    auth_state_path: Option<PathBuf>,
) -> crate::openai_responses_ws::OpenAIResponsesWsClient {
    let auth_provider = Box::new(CodexAuthProvider::new(auth_state, auth_state_path));
    let request_customizer = Box::new(crate::openai_responses_ws::CodexWsRequestCustomizer);

    crate::openai_responses_ws::OpenAIResponsesWsClient::with_customization(
        model,
        CHATGPT_BASE_URL.to_string(),
        auth_provider,
        request_customizer,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pkce_generation() {
        let (verifier, challenge) = generate_pkce_pair();
        assert!(!verifier.is_empty());
        assert!(!challenge.is_empty());
        assert_ne!(verifier, challenge);

        // Verify the challenge is SHA-256 of verifier
        let expected_hash = sha256(verifier.as_bytes());
        let expected_challenge = URL_SAFE_NO_PAD.encode(expected_hash);
        assert_eq!(challenge, expected_challenge);
    }

    #[test]
    fn test_sha256_known_value() {
        // SHA-256 of empty string
        let hash = sha256(b"");
        let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(
            hex,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_sha256_hello() {
        let hash = sha256(b"hello");
        let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(
            hex,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn test_extract_account_id() {
        // Create a fake JWT with the expected claims structure
        let header = URL_SAFE_NO_PAD.encode(b"{}");
        let claims = serde_json::json!({
            "email": "test@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "org_test123",
                "chatgpt_plan_type": "plus",
            }
        });
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let signature = URL_SAFE_NO_PAD.encode(b"fake_signature");
        let token = format!("{}.{}.{}", header, payload, signature);

        assert_eq!(extract_account_id(&token), Some("org_test123".to_string()));
    }

    #[test]
    fn test_extract_user_info() {
        let header = URL_SAFE_NO_PAD.encode(b"{}");
        let claims = serde_json::json!({
            "email": "test@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "org_test123",
                "chatgpt_plan_type": "pro",
            }
        });
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let signature = URL_SAFE_NO_PAD.encode(b"fake_signature");
        let token = format!("{}.{}.{}", header, payload, signature);

        let (email, plan) = extract_user_info(&token);
        assert_eq!(email, Some("test@example.com".to_string()));
        assert_eq!(plan, Some("pro".to_string()));
    }

    #[test]
    fn test_auth_state_serialization() {
        let state = CodexAuthState {
            tokens: CodexTokens {
                id_token: "id.token.here".to_string(),
                access_token: "access.token.here".to_string(),
                refresh_token: "refresh_token_here".to_string(),
                account_id: Some("org_123".to_string()),
            },
            last_refresh: Utc::now(),
        };

        let json = serde_json::to_string(&state).unwrap();
        let deserialized: CodexAuthState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.tokens.account_id, Some("org_123".to_string()));
    }

    #[test]
    fn test_needs_refresh() {
        // Fresh tokens don't need refresh
        let state = CodexAuthState {
            tokens: CodexTokens {
                id_token: "x".to_string(),
                access_token: "x".to_string(),
                refresh_token: "x".to_string(),
                account_id: None,
            },
            last_refresh: Utc::now(),
        };
        assert!(!state.needs_refresh());

        // Old tokens need refresh
        let old_state = CodexAuthState {
            tokens: state.tokens.clone(),
            last_refresh: Utc::now() - chrono::Duration::days(10),
        };
        assert!(old_state.needs_refresh());
    }

    #[test]
    fn test_authorize_url_format() {
        let url = build_authorize_url(
            "test_challenge",
            "test_state",
            "http://localhost:1455/auth/callback",
        );
        assert!(url.contains("auth.openai.com/oauth/authorize"));
        assert!(url.contains("client_id="));
        assert!(url.contains("code_challenge=test_challenge"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=test_state"));
        assert!(url.contains("originator=codex_cli"));
        assert!(url.contains("codex_cli_simplified_flow=true"));
    }
}
