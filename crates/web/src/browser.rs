//! Browser launching with selectable profiles.
//!
//! The `web` crate historically launched a throwaway headless Chromium per
//! [`WebClient`](crate::WebClient). Browser *agency* — an agent that logs in
//! and acts across many steps — needs two more things:
//!
//! - a **persistent** profile, so an authenticated login survives between
//!   launches and can be reused ("act as me"), and
//! - a **headful** window, so a human can perform the login the model must
//!   never do itself.
//!
//! Both live behind one [`BrowserLaunchConfig`]; the ephemeral + headless
//! default preserves the old behavior.

use anyhow::Result;
use chromiumoxide::{Browser, BrowserConfig};
use futures::StreamExt;
use std::path::PathBuf;
use tempfile::TempDir;
use tokio::task::JoinHandle;

/// Where a browser stores cookies / localStorage / session data between
/// launches.
#[derive(Debug, Clone, Default)]
pub enum BrowserProfile {
    /// Throwaway profile in a fresh temp dir, deleted when the launch drops.
    /// The default: right for one-shot research and stateless testing.
    #[default]
    Ephemeral,
    /// A persistent, named user-data-dir. Cookies and logged-in sessions
    /// survive across launches, so an authenticated login can be reused.
    /// The directory is created if it does not exist.
    Persistent(PathBuf),
}

/// How to launch a browser: which profile, and headless vs. headful.
#[derive(Debug, Clone, Default)]
pub struct BrowserLaunchConfig {
    pub profile: BrowserProfile,
    /// Show a real window. Required for the human-in-the-loop login handoff;
    /// headless (the default) is right for autonomous operation.
    pub headful: bool,
}

impl BrowserLaunchConfig {
    /// Persistent, headless — autonomous operation on a reusable profile.
    pub fn persistent(dir: impl Into<PathBuf>) -> Self {
        Self {
            profile: BrowserProfile::Persistent(dir.into()),
            headful: false,
        }
    }

    /// Headful variant of this config (for a login handoff).
    pub fn headful(mut self) -> Self {
        self.headful = true;
        self
    }
}

/// Resolve a profile to the concrete user-data-dir Chromium should use.
///
/// Returns the directory path plus, for an ephemeral profile, the [`TempDir`]
/// guard that must be kept alive for the browser's lifetime (dropping it
/// deletes the profile). A persistent profile returns `None` and is created on
/// disk if missing.
pub(crate) fn resolve_user_data_dir(
    profile: &BrowserProfile,
) -> Result<(PathBuf, Option<TempDir>)> {
    match profile {
        BrowserProfile::Ephemeral => {
            let dir = tempfile::tempdir()?;
            Ok((dir.path().to_path_buf(), Some(dir)))
        }
        BrowserProfile::Persistent(path) => {
            std::fs::create_dir_all(path)?;
            Ok((path.clone(), None))
        }
    }
}

/// A launched browser plus the resources that must outlive it: the temp-dir
/// guard for an ephemeral profile, and the background CDP handler task that
/// drives the connection (aborted on drop).
pub struct LaunchedBrowser {
    pub browser: Browser,
    _user_data_dir: Option<TempDir>,
    handler: JoinHandle<()>,
}

impl LaunchedBrowser {
    /// Launch a Chromium instance for the given config.
    pub async fn launch(config: BrowserLaunchConfig) -> Result<Self> {
        let (data_dir, temp_guard) = resolve_user_data_dir(&config.profile)?;

        let mut builder = BrowserConfig::builder().user_data_dir(&data_dir);
        if config.headful {
            builder = builder.with_head();
        }
        let browser_config = builder.build().map_err(|e| anyhow::anyhow!("{e}"))?;

        let (browser, mut handler) = Browser::launch(browser_config).await?;
        // Drain the handler stream to drive the CDP connection. We do not log
        // per-event errors: chromiumoxide already emits them via `tracing`, and
        // recent Chrome versions send CDP messages this version can't
        // deserialize ("data did not match any variant of untagged enum
        // Message") — benign noise we must not duplicate to stderr.
        let handler = tokio::spawn(async move { while handler.next().await.is_some() {} });

        Ok(Self {
            browser,
            _user_data_dir: temp_guard,
            handler,
        })
    }

    /// Close the browser gracefully and wait for the process to exit, so a
    /// persistent profile flushes its cookie store to disk. Chromium only
    /// persists cookies on a clean shutdown, and the flush happens as the
    /// process exits — hence the `wait` after `close`. Best-effort.
    pub async fn close(&mut self) {
        let _ = self.browser.close().await;
        let _ = self.browser.wait().await;
    }
}

impl Drop for LaunchedBrowser {
    fn drop(&mut self) {
        self.handler.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ephemeral_profile_yields_a_fresh_existing_dir_with_a_guard() {
        let (path_a, guard_a) = resolve_user_data_dir(&BrowserProfile::Ephemeral).unwrap();
        assert!(path_a.exists(), "temp profile dir should be created");
        assert!(
            guard_a.is_some(),
            "ephemeral profile must return a TempDir guard"
        );

        // A second ephemeral profile is a distinct directory.
        let (path_b, _guard_b) = resolve_user_data_dir(&BrowserProfile::Ephemeral).unwrap();
        assert_ne!(path_a, path_b, "each ephemeral profile is its own dir");

        // Dropping the guard removes the directory.
        drop(guard_a);
        assert!(!path_a.exists(), "dropping the guard deletes the profile");
    }

    #[test]
    fn persistent_profile_creates_and_reuses_the_named_dir() {
        let base = tempfile::tempdir().unwrap();
        let profile_dir = base.path().join("profiles").join("elster");
        let profile = BrowserProfile::Persistent(profile_dir.clone());

        let (path, guard) = resolve_user_data_dir(&profile).unwrap();
        assert_eq!(path, profile_dir);
        assert!(path.exists(), "persistent dir should be created if missing");
        assert!(guard.is_none(), "persistent profile has no temp guard");

        // Resolving again returns the same path (reuse, not a fresh dir).
        let (path_again, _) = resolve_user_data_dir(&profile).unwrap();
        assert_eq!(path, path_again);
    }
}
