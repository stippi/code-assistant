//! Where tokens come from. `GH_TOKEN` / `GITHUB_TOKEN` when set, else the
//! GitHub CLI's login (`gh auth token`). Tokens are cached for five minutes
//! and never logged.

use crate::error::{Error, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

const TOKEN_TTL: Duration = Duration::from_secs(5 * 60);
/// A missing login is asked again soon, but not for every request.
const MISSING_TTL: Duration = Duration::from_secs(10);
const GH_TIMEOUT: Duration = Duration::from_secs(10);

/// Supplies the token for a host.
#[async_trait]
pub trait TokenSource: Send + Sync {
    async fn token(&self, host: &str) -> Result<String>;
}

/// The environment, then `gh auth token --hostname <host>`.
#[derive(Debug, Default)]
pub struct GhTokenSource;

#[async_trait]
impl TokenSource for GhTokenSource {
    async fn token(&self, host: &str) -> Result<String> {
        let env: HashMap<String, String> = std::env::vars().collect();
        if let Some(token) = environment_token(host, &env) {
            return Ok(token);
        }
        gh_auth_token(host).await
    }
}

/// One fixed token for every host.
pub struct StaticTokenSource(pub String);

#[async_trait]
impl TokenSource for StaticTokenSource {
    async fn token(&self, _host: &str) -> Result<String> {
        Ok(self.0.clone())
    }
}

/// `GH_TOKEN` / `GITHUB_TOKEN` belong to github.com (and GHE.com tenancies);
/// `GH_ENTERPRISE_TOKEN` only to the server `GH_HOST` names, so a remote URL
/// pointing elsewhere cannot draw it out.
pub(crate) fn environment_token(host: &str, env: &HashMap<String, String>) -> Option<String> {
    let names: &[&str] = if host == "github.com" || host.ends_with(".ghe.com") {
        &["GH_TOKEN", "GITHUB_TOKEN"]
    } else if env
        .get("GH_HOST")
        .is_some_and(|gh_host| gh_host.trim().eq_ignore_ascii_case(host))
    {
        &["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"]
    } else {
        &[]
    };
    names
        .iter()
        .filter_map(|name| env.get(*name))
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
        .map(str::to_string)
}

async fn gh_auth_token(host: &str) -> Result<String> {
    let no_credential = |detail: &str| Error::NoCredential {
        host: host.to_string(),
        detail: detail.to_string(),
    };
    let child = tokio::process::Command::new("gh")
        .args(["auth", "token", "--hostname", host])
        // Never let gh print the token into a debug log or prompt.
        .env("GH_DEBUG", "")
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                no_credential("set GH_TOKEN, or install the GitHub CLI and run `gh auth login`")
            }
            _ => no_credential(&format!("could not run the GitHub CLI: {e}")),
        })?;
    let output = tokio::time::timeout(GH_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| no_credential("`gh auth token` did not answer within 10 s"))?
        .map_err(|e| no_credential(&format!("could not run the GitHub CLI: {e}")))?;
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || token.is_empty() {
        return Err(no_credential(&format!(
            "run `gh auth login --hostname {host}`"
        )));
    }
    Ok(token)
}

struct Held {
    token: Result<String>,
    until: Instant,
}

/// Remembers each host's token, so `gh` runs once per five minutes rather
/// than once per request.
pub(crate) struct TokenCache {
    source: Arc<dyn TokenSource>,
    held: Mutex<HashMap<String, Held>>,
}

impl TokenCache {
    pub fn new(source: Arc<dyn TokenSource>) -> Self {
        Self {
            source,
            held: Mutex::new(HashMap::new()),
        }
    }

    pub async fn get(&self, host: &str) -> Result<String> {
        // Held across the lookup: concurrent requests wait for one `gh` run.
        let mut held = self.held.lock().await;
        let now = Instant::now();
        if let Some(entry) = held.get(host)
            && entry.until > now
        {
            return entry.token.clone();
        }
        let token = self.source.token(host).await;
        let ttl = if token.is_ok() {
            TOKEN_TTL
        } else {
            MISSING_TTL
        };
        held.insert(
            host.to_string(),
            Held {
                token: token.clone(),
                until: now + ttl,
            },
        );
        token
    }

    /// Forgets a token GitHub refused; the source may hold a newer one.
    pub async fn invalidate(&self, host: &str) {
        self.held.lock().await.remove(host);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting {
        calls: AtomicUsize,
        fail: bool,
    }

    #[async_trait]
    impl TokenSource for Counting {
        async fn token(&self, host: &str) -> Result<String> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail {
                Err(Error::NoCredential {
                    host: host.into(),
                    detail: "none".into(),
                })
            } else {
                Ok(format!("token-{n}"))
            }
        }
    }

    fn cache(fail: bool) -> (Arc<Counting>, TokenCache) {
        let source = Arc::new(Counting {
            calls: AtomicUsize::new(0),
            fail,
        });
        (source.clone(), TokenCache::new(source))
    }

    #[tokio::test(start_paused = true)]
    async fn keeps_a_token_for_five_minutes_per_host() {
        let (source, cache) = cache(false);
        assert_eq!(cache.get("github.com").await.unwrap(), "token-1");
        tokio::time::advance(Duration::from_secs(299)).await;
        assert_eq!(cache.get("github.com").await.unwrap(), "token-1");
        assert_eq!(cache.get("ghe.example").await.unwrap(), "token-2");
        tokio::time::advance(Duration::from_secs(2)).await;
        assert_eq!(cache.get("github.com").await.unwrap(), "token-3");
        assert_eq!(source.calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn asks_again_after_invalidation() {
        let (_, cache) = cache(false);
        assert_eq!(cache.get("github.com").await.unwrap(), "token-1");
        cache.invalidate("github.com").await;
        assert_eq!(cache.get("github.com").await.unwrap(), "token-2");
    }

    #[tokio::test(start_paused = true)]
    async fn remembers_a_missing_login_only_briefly() {
        let (source, cache) = cache(true);
        assert!(cache.get("github.com").await.is_err());
        assert!(cache.get("github.com").await.is_err());
        assert_eq!(source.calls.load(Ordering::SeqCst), 1);
        tokio::time::advance(MISSING_TTL).await;
        assert!(cache.get("github.com").await.is_err());
        assert_eq!(source.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn environment_tokens_stay_with_their_hosts() {
        let env: HashMap<String, String> = [
            ("GH_TOKEN", " "),
            ("GITHUB_TOKEN", "dotcom"),
            ("GH_ENTERPRISE_TOKEN", "enterprise"),
            ("GH_HOST", "ghe.corp"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert_eq!(
            environment_token("github.com", &env).as_deref(),
            Some("dotcom")
        );
        assert_eq!(
            environment_token("acme.ghe.com", &env).as_deref(),
            Some("dotcom")
        );
        assert_eq!(
            environment_token("ghe.corp", &env).as_deref(),
            Some("enterprise")
        );
        assert_eq!(environment_token("other.corp", &env), None);
    }
}
