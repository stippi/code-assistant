//! OAuth authorization support for HTTP (streamable) MCP servers.
//!
//! MCP's authorization spec is an OAuth 2.1 authorization-code flow with PKCE
//! and discovery (RFC 9728 protected-resource metadata → RFC 8414
//! authorization-server metadata). A server that requires it answers the
//! `initialize` request with `401` and a `WWW-Authenticate` challenge; the
//! client then discovers the authorization server, runs a browser consent
//! flow, and reconnects with a bearer token.
//!
//! This module keeps the pieces that a client needs but that are not baked
//! into the transport:
//!
//! * [`FileCredentialStore`] — persists one server's tokens to disk so the
//!   interactive login survives restarts (implements rmcp's
//!   [`CredentialStore`]).
//! * [`OAuthAuthorizer`] — the embedder-provided seam that presents the
//!   authorization URL to the user (open a browser) and returns the redirect
//!   callback parameters.
//!
//! The interactive login itself lives in [`crate::client`]; token *reuse* is
//! non-interactive and happens transparently when connecting.

use async_trait::async_trait;
use rmcp::transport::auth::{AuthError, CredentialStore, StoredCredentials};
use std::path::{Path, PathBuf};

/// A [`CredentialStore`] that persists a single MCP server's OAuth tokens to a
/// JSON file. Each server gets its own file (the embedder picks the path,
/// typically `<config_dir>/mcp-oauth/<server>.json`), so one store instance
/// backs exactly one server's credentials.
///
/// Writes are atomic (write-to-temp-then-rename) and, on Unix, the file is
/// created with `0600` permissions since it holds bearer/refresh tokens.
#[derive(Debug, Clone)]
pub struct FileCredentialStore {
    path: PathBuf,
}

impl FileCredentialStore {
    /// Back this store with the file at `path`. The file and its parent
    /// directory are created lazily on the first `save`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The file this store reads and writes.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn store_err(context: &str, error: impl std::fmt::Display) -> AuthError {
    AuthError::CredentialStoreError(format!("{context}: {error}"))
}

#[async_trait]
impl CredentialStore for FileCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(store_err(
                    &format!("reading {}", self.path.display()),
                    error,
                ));
            }
        };
        let credentials = serde_json::from_slice(&bytes)
            .map_err(|error| store_err(&format!("parsing {}", self.path.display()), error))?;
        Ok(Some(credentials))
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| store_err(&format!("creating {}", parent.display()), error))?;
        }
        let json = serde_json::to_vec_pretty(&credentials)
            .map_err(|error| store_err("serializing credentials", error))?;

        // Write to a sibling temp file, then rename over the target so a
        // reader never sees a half-written file.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)
            .map_err(|error| store_err(&format!("writing {}", tmp.display()), error))?;
        restrict_permissions(&tmp)?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|error| store_err(&format!("renaming into {}", self.path.display()), error))?;
        Ok(())
    }

    async fn clear(&self) -> Result<(), AuthError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(store_err(
                &format!("removing {}", self.path.display()),
                error,
            )),
        }
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), AuthError> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms)
        .map_err(|error| store_err(&format!("chmod {}", path.display()), error))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), AuthError> {
    Ok(())
}

/// The parameters an authorization server returns to the OAuth redirect URI
/// after the user consents.
#[derive(Debug, Clone)]
pub struct AuthorizationOutcome {
    /// The authorization code to exchange for tokens.
    pub code: String,
    /// The CSRF `state` value the server echoed back.
    pub state: String,
    /// The optional RFC 9207 `iss` (issuer) parameter, validated when present.
    pub issuer: Option<String>,
}

/// The embedder-provided half of the interactive OAuth flow: present the
/// authorization URL to the user (typically by opening a browser and running
/// a loopback redirect server) and return the callback parameters.
///
/// The MCP client crate drives discovery, PKCE, token exchange and reconnect;
/// it only needs the embedder to handle the human-facing browser round-trip,
/// which is why this is a seam rather than baked in.
#[async_trait]
pub trait OAuthAuthorizer: Send + Sync {
    /// The redirect URI the authorization server should send the user back to
    /// (e.g. `http://127.0.0.1:8117/callback`). Must match what the loopback
    /// server in [`Self::authorize`] listens on.
    fn redirect_uri(&self) -> String;

    /// Present `authorization_url` to the user and resolve once the redirect
    /// callback has been received.
    async fn authorize(&self, authorization_url: String) -> anyhow::Result<AuthorizationOutcome>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::transport::auth::StoredCredentials;

    fn sample_credentials() -> StoredCredentials {
        StoredCredentials::new(
            "client-123".to_string(),
            None,
            vec!["mcp:read".to_string()],
            Some(1_700_000_000),
        )
        .with_issuer(Some("https://issuer.example.com".to_string()))
    }

    #[tokio::test]
    async fn load_returns_none_when_file_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path().join("nested/server.json"));
        assert!(store.load().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn save_then_load_round_trips_and_creates_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path().join("mcp-oauth/sap.json"));
        store.save(sample_credentials()).await.unwrap();

        let loaded = store.load().await.unwrap().expect("credentials present");
        assert_eq!(loaded.client_id, "client-123");
        assert_eq!(loaded.granted_scopes, vec!["mcp:read".to_string()]);
        assert_eq!(loaded.issuer.as_deref(), Some("https://issuer.example.com"));
    }

    #[tokio::test]
    async fn clear_removes_the_file_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path().join("server.json"));
        store.save(sample_credentials()).await.unwrap();
        assert!(store.path().exists());

        store.clear().await.unwrap();
        assert!(!store.path().exists());
        // Clearing an absent file is a no-op, not an error.
        store.clear().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn saved_file_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path().join("server.json"));
        store.save(sample_credentials()).await.unwrap();

        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "group/other bits must be clear: {mode:o}");
    }
}
