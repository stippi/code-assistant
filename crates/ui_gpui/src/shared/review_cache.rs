//! On-disk cache for Review panel scan results.
//!
//! One JSON file per `(repo, mode)` under `<config_dir>/review-cache/`. The
//! cache exists purely so the panel can show the last known state instantly
//! while a fresh background scan runs — entries are never trusted as current
//! and are always refreshed after being displayed.

use code_assistant_core::session::{RepoReview, ReviewMode, ReviewScanState};
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Serialized form of one repo's last scan result. `repo_root` and `mode` are
/// stored so a hash collision can be detected on load.
#[derive(Debug, Serialize, Deserialize)]
struct CachedRepoScan {
    repo_root: PathBuf,
    mode: ReviewMode,
    current_branch: Option<String>,
    base_candidates: Vec<String>,
    base: Option<String>,
    files: Vec<git::ChangedFile>,
    stats: git::DiffStats,
}

fn cache_dir() -> PathBuf {
    code_assistant_core::config_dir::config_dir().join("review-cache")
}

fn cache_path(repo_root: &Path, mode: ReviewMode) -> PathBuf {
    let mut hasher = std::hash::DefaultHasher::new();
    repo_root.hash(&mut hasher);
    mode.hash(&mut hasher);
    cache_dir().join(format!("{:016x}.json", hasher.finish()))
}

/// Load the cached scan for `(repo_root, mode)`, returned as a
/// [`ReviewScanState::Pending`] entry (data present, but not fresh).
/// Returns `None` when there is no valid cache entry.
pub fn load(repo_root: &Path, label: &str, mode: ReviewMode) -> Option<RepoReview> {
    let path = cache_path(repo_root, mode);
    let json = std::fs::read_to_string(&path).ok()?;
    let cached: CachedRepoScan = match serde_json::from_str(&json) {
        Ok(c) => c,
        Err(e) => {
            warn!("Ignoring corrupt review cache {}: {}", path.display(), e);
            return None;
        }
    };
    // Guard against hash collisions and stale mode mixups.
    if cached.repo_root != repo_root || cached.mode != mode {
        return None;
    }
    Some(RepoReview {
        repo_root: cached.repo_root,
        label: label.to_owned(),
        current_branch: cached.current_branch,
        base_candidates: cached.base_candidates,
        base: cached.base,
        files: cached.files,
        stats: cached.stats,
        scan_state: ReviewScanState::Pending,
    })
}

/// Persist a finished scan for later instant display. Errors are logged only —
/// the cache is an optimization, never a requirement.
pub fn store(review: &RepoReview, mode: ReviewMode) {
    let cached = CachedRepoScan {
        repo_root: review.repo_root.clone(),
        mode,
        current_branch: review.current_branch.clone(),
        base_candidates: review.base_candidates.clone(),
        base: review.base.clone(),
        files: review.files.clone(),
        stats: review.stats,
    };
    let path = cache_path(&review.repo_root, mode);
    match code_assistant_core::utils::file_utils::atomic_write_json(&path, &cached) {
        Ok(()) => debug!("Cached review scan to {}", path.display()),
        Err(e) => warn!("Failed to write review cache {}: {}", path.display(), e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Point the config dir at a temp dir for the duration of a test.
    /// Serialized via a lock because the env var is process-global.
    fn with_temp_config_dir<R>(f: impl FnOnce() -> R) -> R {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: guarded by LOCK; tests touching this var run serialized.
        unsafe { std::env::set_var("CODE_ASSISTANT_CONFIG_DIR", dir.path()) };
        let result = f();
        unsafe { std::env::remove_var("CODE_ASSISTANT_CONFIG_DIR") };
        result
    }

    fn sample_review(root: &Path) -> RepoReview {
        RepoReview {
            repo_root: root.to_path_buf(),
            label: "alpha".into(),
            current_branch: Some("feature".into()),
            base_candidates: vec!["main".into(), "origin/main".into()],
            base: Some("origin/main".into()),
            files: vec![git::ChangedFile {
                path: "src/lib.rs".into(),
                orig_path: None,
                status: git::ChangeStatus::Modified,
            }],
            stats: git::DiffStats {
                additions: 12,
                deletions: 3,
            },
            scan_state: ReviewScanState::Done,
        }
    }

    #[test]
    fn store_load_roundtrip_marks_pending() {
        with_temp_config_dir(|| {
            let root = PathBuf::from("/tmp/some/repo");
            let review = sample_review(&root);
            store(&review, ReviewMode::WorkingTree);

            let loaded = load(&root, "alpha", ReviewMode::WorkingTree).expect("cache hit");
            assert_eq!(loaded.scan_state, ReviewScanState::Pending);
            assert_eq!(loaded.files, review.files);
            assert_eq!(loaded.stats, review.stats);
            assert_eq!(loaded.base.as_deref(), Some("origin/main"));

            // Different mode is a separate cache entry.
            assert!(load(&root, "alpha", ReviewMode::BranchVsBase).is_none());
            // Unknown repo misses.
            assert!(load(Path::new("/tmp/other"), "x", ReviewMode::WorkingTree).is_none());
        });
    }
}
