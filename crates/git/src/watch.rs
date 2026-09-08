//! Watch git working directories for changes that could alter a review.
//!
//! A [`ChangeWatcher`] watches each repo's working directory recursively (plus
//! the git dirs of linked worktrees, which live outside it) and calls back at
//! most once per quiet period. It answers only "something may have changed" —
//! the consumer re-lists and uses fingerprints to find out what.
//!
//! Events inside a `.git` directory are filtered the way Zed's worktree
//! scanner does: churn that never changes what `git status` or `git diff`
//! report (object writes, hooks, reflogs, lock files, temp files, …) is
//! dropped, while `index`, `HEAD`, refs and the like go through.

use anyhow::{Context, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tracing::{debug, trace, warn};

/// Files in a git dir whose changes never affect status or diffs.
const SKIPPED_FILE_NAMES_IN_DOT_GIT: [&str; 5] = [
    "COMMIT_EDITMSG",
    "FETCH_HEAD",
    "ORIG_HEAD",
    "BISECT_LOG",
    "gc.pid",
];

/// Subdirectories of a git dir whose churn never affects status or diffs.
const SKIPPED_DIRS_IN_DOT_GIT: [&str; 7] = [
    "fsmonitor--daemon",
    "lfs",
    "objects",
    "hooks",
    "rebase-merge",
    "rebase-apply",
    "sequencer",
];

const LOGS_DIR: &str = "logs";
const LOGS_REF_STASH: &str = "logs/refs/stash";
const INFO_DIR: &str = "info";
const REPO_EXCLUDE: &str = "info/exclude";

/// Watches repositories and reports "something may have changed".
///
/// Dropping the watcher stops it; a callback already in progress finishes.
pub struct ChangeWatcher {
    _watcher: RecommendedWatcher,
}

/// What to watch for one repository, and which directories count as git
/// dirs for event filtering.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WatchTargets {
    /// Directories to watch recursively.
    paths: Vec<PathBuf>,
    /// Git dirs (private + common) — events under these are filtered.
    git_dirs: Vec<PathBuf>,
}

impl ChangeWatcher {
    /// Start watching `repo_roots`. `on_change` runs on a background thread
    /// once a burst of events has been quiet for `debounce` — or after
    /// `4 × debounce` of continuous activity, so a long build still yields
    /// periodic refreshes.
    pub fn start(
        repo_roots: &[PathBuf],
        debounce: Duration,
        on_change: impl Fn() + Send + 'static,
    ) -> Result<Self> {
        let mut paths = Vec::new();
        let mut git_dirs = Vec::new();
        for root in repo_roots {
            let targets = watch_targets(root)?;
            paths.extend(targets.paths);
            git_dirs.extend(targets.git_dirs);
        }

        // Each interesting event is one `()` on this channel; the debounce
        // thread turns bursts into single callbacks. Dropping the notify
        // watcher drops the sender, which ends the thread.
        let (tx, rx) = mpsc::channel::<()>();
        let filter_git_dirs = git_dirs.clone();
        let mut watcher =
            notify::recommended_watcher(move |res: Result<Event, notify::Error>| match res {
                Ok(event) => {
                    if event.paths.iter().any(|p| !is_noise(p, &filter_git_dirs)) {
                        trace!("Review watcher: relevant event {:?}", event);
                        let _ = tx.send(());
                    }
                }
                Err(e) => warn!("Review watcher error: {e}"),
            })
            .context("Failed to create filesystem watcher")?;

        for path in &paths {
            watcher
                .watch(path, RecursiveMode::Recursive)
                .with_context(|| format!("Failed to watch {}", path.display()))?;
            debug!("Review watcher: watching {}", path.display());
        }

        std::thread::Builder::new()
            .name("review-change-watcher".into())
            .spawn(move || debounce_loop(rx, debounce, on_change))
            .context("Failed to spawn watcher debounce thread")?;

        Ok(Self { _watcher: watcher })
    }
}

/// Coalesce event bursts: fire once the channel has been quiet for
/// `debounce`, but no later than `4 × debounce` after the burst began.
fn debounce_loop(rx: mpsc::Receiver<()>, debounce: Duration, on_change: impl Fn()) {
    let max_wait = debounce * 4;
    // Block until the first event of a burst; a closed channel ends the loop.
    while rx.recv().is_ok() {
        let deadline = Instant::now() + max_wait;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(debounce.min(remaining)) {
                Ok(()) if Instant::now() < deadline => continue,
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
        on_change();
    }
}

/// The paths to watch for one repo root: the working directory, plus the
/// private and common git dirs when they live outside it (linked worktrees).
fn watch_targets(root: &Path) -> Result<WatchTargets> {
    let repo = crate::GitRepository::open(root)
        .with_context(|| format!("Failed to open git repository at {}", root.display()))?;
    // Canonical paths: gix reports a linked worktree's common dir as
    // `.git/worktrees/<name>/../..`, and event paths must prefix-match.
    let canonical = |p: PathBuf| p.canonicalize().unwrap_or(p);
    let workdir = canonical(repo.workdir().to_path_buf());
    let git_dir = canonical(repo.gitdir());
    let common_dir = canonical(repo.commondir());

    let mut paths = vec![workdir.clone()];
    for dir in [&git_dir, &common_dir] {
        if !dir.starts_with(&workdir) && !paths.contains(dir) {
            paths.push(dir.clone());
        }
    }
    let mut git_dirs = vec![git_dir];
    if !git_dirs.contains(&common_dir) {
        git_dirs.push(common_dir);
    }
    Ok(WatchTargets { paths, git_dirs })
}

/// True if an event at `path` cannot change what a review shows. Paths
/// outside any git dir are never noise.
fn is_noise(path: &Path, git_dirs: &[PathBuf]) -> bool {
    match path_in_git_dir(path, git_dirs) {
        Some(inner) => is_git_dir_noise(&inner),
        None => false,
    }
}

/// If `path` is inside a git dir, return its path relative to that dir. Known
/// git dirs (which may lie outside the working directory) are checked first,
/// then any `.git` ancestor — which also covers nested repositories.
fn path_in_git_dir(path: &Path, git_dirs: &[PathBuf]) -> Option<PathBuf> {
    if let Some(inner) = git_dirs
        .iter()
        .filter_map(|dir| path.strip_prefix(dir).ok())
        .min_by_key(|inner| inner.components().count())
    {
        return Some(inner.to_path_buf());
    }
    path.ancestors()
        .find(|a| a.file_name().is_some_and(|n| n == ".git"))
        .and_then(|dot_git| path.strip_prefix(dot_git).ok())
        .map(Path::to_path_buf)
}

/// Zed's filter for events inside a git dir (`inner` is relative to it).
/// An empty `inner` is the git dir itself, whose own metadata changes are
/// irrelevant.
fn is_git_dir_noise(inner: &Path) -> bool {
    if inner.as_os_str().is_empty() {
        return true;
    }
    let file_name = inner.file_name().and_then(|n| n.to_str());
    let extension = inner.extension().and_then(|e| e.to_str());

    SKIPPED_FILE_NAMES_IN_DOT_GIT
        .iter()
        .any(|skipped| file_name == Some(skipped))
        || (inner.starts_with(LOGS_DIR) && inner != Path::new(LOGS_REF_STASH))
        || (inner.starts_with(INFO_DIR) && inner != Path::new(REPO_EXCLUDE))
        || SKIPPED_DIRS_IN_DOT_GIT
            .iter()
            .any(|skipped| inner.starts_with(skipped))
        || extension == Some("lock")
        || (inner.components().count() == 1 && matches!(extension, Some("new") | Some("tmp")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::init_repo_with_commit;
    use tempfile::TempDir;

    fn noise(inner: &str) -> bool {
        is_git_dir_noise(Path::new(inner))
    }

    #[test]
    fn git_dir_filter_drops_churn_and_keeps_state() {
        // Dropped: the dir itself, objects, hooks, reflogs, locks, temp files.
        assert!(noise(""));
        assert!(noise("objects/ab/cdef"));
        assert!(noise("objects/pack/pack-1.idx"));
        assert!(noise("hooks/pre-commit"));
        assert!(noise("lfs/objects/x"));
        assert!(noise("fsmonitor--daemon/cookie"));
        assert!(noise("rebase-merge/done"));
        assert!(noise("rebase-apply/patch"));
        assert!(noise("sequencer/todo"));
        assert!(noise("logs/HEAD"));
        assert!(noise("logs/refs/heads/main"));
        assert!(noise("info/refs"));
        assert!(noise("COMMIT_EDITMSG"));
        assert!(noise("FETCH_HEAD"));
        assert!(noise("ORIG_HEAD"));
        assert!(noise("BISECT_LOG"));
        assert!(noise("gc.pid"));
        assert!(noise("index.lock"));
        assert!(noise("refs/heads/main.lock"));
        assert!(noise("index.tmp"));
        assert!(noise("packed-refs.new"));

        // Kept: everything that feeds status and diffs.
        assert!(!noise("index"));
        assert!(!noise("HEAD"));
        assert!(!noise("refs/heads/main"));
        assert!(!noise("packed-refs"));
        assert!(!noise("MERGE_HEAD"));
        assert!(!noise("logs/refs/stash"));
        assert!(!noise("info/exclude"));
        assert!(!noise("config"));
        // Temp-suffix rule only applies at the top level of the git dir.
        assert!(!noise("refs/heads/feature.tmp"));
    }

    #[test]
    fn locates_git_dir_by_prefix_or_dot_git_ancestor() {
        let linked = PathBuf::from("/main/.git/worktrees/wt");
        let git_dirs = vec![linked.clone(), PathBuf::from("/main/.git")];

        // Explicit git dirs win, most specific first.
        assert_eq!(
            path_in_git_dir(&linked.join("index"), &git_dirs),
            Some(PathBuf::from("index"))
        );
        assert_eq!(
            path_in_git_dir(Path::new("/main/.git/refs/heads/x"), &git_dirs),
            Some(PathBuf::from("refs/heads/x"))
        );
        // A nested repo's `.git` is found via the ancestor rule.
        assert_eq!(
            path_in_git_dir(Path::new("/wt/vendor/lib/.git/objects/aa"), &git_dirs),
            Some(PathBuf::from("objects/aa"))
        );
        // Regular working-tree files are outside any git dir.
        assert_eq!(
            path_in_git_dir(Path::new("/wt/src/lib.rs"), &git_dirs),
            None
        );
        assert!(!is_noise(Path::new("/wt/src/lib.rs"), &git_dirs));
        assert!(is_noise(Path::new("/main/.git/objects/aa/bb"), &git_dirs));
    }

    #[test]
    fn watch_targets_cover_linked_worktree_git_dirs() {
        let dir = TempDir::new().unwrap();
        init_repo_with_commit(dir.path());
        let main = dir.path().canonicalize().unwrap();

        let targets = watch_targets(&main).unwrap();
        assert_eq!(targets.paths, vec![main.clone()]);
        assert_eq!(targets.git_dirs, vec![main.join(".git")]);

        // A linked worktree keeps its index/HEAD in the main repo's `.git`.
        let wt = dir.path().join("wt");
        let status = std::process::Command::new("git")
            .args(["worktree", "add", "-b", "wt", wt.to_str().unwrap()])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(status.success());
        let wt = wt.canonicalize().unwrap();

        let targets = watch_targets(&wt).unwrap();
        let private = main.join(".git/worktrees/wt");
        assert_eq!(targets.paths, vec![wt, private.clone(), main.join(".git")]);
        assert_eq!(targets.git_dirs, vec![private, main.join(".git")]);
    }

    #[test]
    fn reports_working_tree_writes_once_per_burst() {
        let dir = TempDir::new().unwrap();
        init_repo_with_commit(dir.path());
        let root = dir.path().canonicalize().unwrap();

        let (tx, rx) = mpsc::channel();
        let _watcher = ChangeWatcher::start(
            std::slice::from_ref(&root),
            Duration::from_millis(100),
            move || {
                let _ = tx.send(());
            },
        )
        .unwrap();
        // Let the OS-level watch settle before producing events.
        std::thread::sleep(Duration::from_millis(300));

        for i in 0..5 {
            std::fs::write(root.join("a.txt"), format!("edit {i}\n")).unwrap();
        }
        rx.recv_timeout(Duration::from_secs(5))
            .expect("a working-tree write must be reported");

        // The burst is coalesced: after it settles, no further callbacks.
        while rx.recv_timeout(Duration::from_millis(600)).is_ok() {}

        // Object churn inside `.git` is filtered out entirely.
        std::fs::create_dir_all(root.join(".git/objects/ab")).unwrap();
        std::fs::write(root.join(".git/objects/ab/cdef"), b"blob").unwrap();
        assert!(
            rx.recv_timeout(Duration::from_millis(800)).is_err(),
            "object writes must not trigger a callback"
        );

        // But a ref update (e.g. a commit) is reported.
        std::fs::write(root.join(".git/refs/heads/topic"), b"0000\n").unwrap();
        rx.recv_timeout(Duration::from_secs(5))
            .expect("a ref write must be reported");
    }
}
