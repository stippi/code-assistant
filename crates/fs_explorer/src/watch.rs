//! Watch a project tree and report which paths changed.
//!
//! A [`TreeWatcher`] watches a root recursively and, once a burst of events
//! has been quiet for the debounce period (or after four periods of
//! continuous activity), calls back with the changed paths relative to the
//! root. Events inside `.git` directories are dropped.

use anyhow::{Context, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tracing::warn;

/// Watches a directory tree. Dropping it stops watching; a callback already
/// in progress finishes.
pub struct TreeWatcher {
    _watcher: RecommendedWatcher,
}

impl TreeWatcher {
    /// Start watching `root`. `on_change` runs on a background thread with
    /// the changed paths, relative to `root`.
    pub fn start(
        root: &Path,
        debounce: Duration,
        on_change: impl Fn(BTreeSet<PathBuf>) + Send + 'static,
    ) -> Result<Self> {
        // Event paths are canonical (on macOS `/private/var/…` for
        // `/var/…`), so both spellings of the root are stripped.
        let roots: Vec<PathBuf> = [Some(root.to_path_buf()), root.canonicalize().ok()]
            .into_iter()
            .flatten()
            .collect();
        let (tx, rx) = mpsc::channel::<PathBuf>();
        let mut watcher =
            notify::recommended_watcher(move |res: Result<Event, notify::Error>| match res {
                Ok(event) => {
                    for path in event.paths {
                        if let Some(rel) = relative(&path, &roots)
                            && !rel.components().any(|c| c.as_os_str() == ".git")
                        {
                            let _ = tx.send(rel);
                        }
                    }
                }
                Err(e) => warn!("Tree watcher error: {e}"),
            })
            .context("Failed to create filesystem watcher")?;
        watcher
            .watch(root, RecursiveMode::Recursive)
            .with_context(|| format!("Failed to watch {}", root.display()))?;
        std::thread::Builder::new()
            .name("tree-watcher".into())
            .spawn(move || debounce_loop(rx, debounce, on_change))
            .context("Failed to spawn watcher thread")?;
        Ok(Self { _watcher: watcher })
    }
}

fn relative(path: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
    roots
        .iter()
        .find_map(|root| path.strip_prefix(root).ok())
        .map(Path::to_path_buf)
}

/// Collect paths until the channel has been quiet for `debounce` (at most
/// `4 × debounce` after the first), then report them.
fn debounce_loop(
    rx: mpsc::Receiver<PathBuf>,
    debounce: Duration,
    on_change: impl Fn(BTreeSet<PathBuf>),
) {
    let max_wait = debounce * 4;
    while let Ok(first) = rx.recv() {
        let mut changed = BTreeSet::from([first]);
        let deadline = Instant::now() + max_wait;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(debounce.min(remaining)) {
                Ok(path) => {
                    changed.insert(path);
                    if Instant::now() >= deadline {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
        on_change(changed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn reports_changed_paths_relative_to_the_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let seen = Arc::new(Mutex::new(BTreeSet::new()));
        let sink = seen.clone();
        let _watcher = TreeWatcher::start(dir.path(), Duration::from_millis(50), move |paths| {
            sink.lock().unwrap().extend(paths);
        })
        .unwrap();
        // FSEvents may deliver events from just before the watch started;
        // give it a moment to settle before writing.
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(dir.path().join("src/a.rs"), "x").unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while !seen.lock().unwrap().contains(Path::new("src/a.rs")) {
            assert!(Instant::now() < deadline, "no event for src/a.rs");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn relative_strips_either_root() {
        let roots = vec![PathBuf::from("/var/x"), PathBuf::from("/private/var/x")];
        assert_eq!(
            relative(Path::new("/private/var/x/a/b"), &roots),
            Some(PathBuf::from("a/b"))
        );
        assert_eq!(relative(Path::new("/other"), &roots), None);
    }
}
