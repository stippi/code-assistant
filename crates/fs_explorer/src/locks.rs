//! Process-wide write locks keyed by file path.
//!
//! Several agents (a parent and its sub-agents) can work in the same project
//! tree at once, each through its own [`crate::Explorer`]. A read-modify-write
//! of one file must not interleave with another, or the later write silently
//! drops the earlier one. Every writer takes the lock of the path it changes
//! for the whole read, apply, format and read-back sequence.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

type Registry = Mutex<HashMap<PathBuf, Weak<AsyncMutex<()>>>>;

fn registry() -> &'static Registry {
    static LOCKS: OnceLock<Registry> = OnceLock::new();
    LOCKS.get_or_init(Default::default)
}

/// Held while a file is being changed; dropping it releases the path.
pub struct PathLock {
    _guards: Vec<OwnedMutexGuard<()>>,
}

fn key(path: &Path) -> PathBuf {
    // Both spellings of a path (through a symlink, or relative) must map to
    // one lock; a path that does not exist yet keys on its parent's real path.
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => parent
            .canonicalize()
            .map(|parent| parent.join(name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

fn mutex_for(key: PathBuf) -> Arc<AsyncMutex<()>> {
    let mut locks = registry().lock().unwrap();
    if let Some(existing) = locks.get(&key).and_then(Weak::upgrade) {
        return existing;
    }
    // Forget entries whose last holder is gone, so the map stays small.
    locks.retain(|_, lock| lock.strong_count() > 0);
    let lock = Arc::new(AsyncMutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    lock
}

/// Wait for exclusive write access to `path`.
pub async fn lock_path(path: &Path) -> PathLock {
    lock_paths([path]).await
}

/// Wait for exclusive write access to all `paths` at once. Locks are taken
/// in a fixed order, so two callers with overlapping sets cannot deadlock.
pub async fn lock_paths<P: AsRef<Path>>(paths: impl IntoIterator<Item = P>) -> PathLock {
    let mut keys: Vec<PathBuf> = paths.into_iter().map(|p| key(p.as_ref())).collect();
    keys.sort();
    keys.dedup();
    let mut guards = Vec::with_capacity(keys.len());
    for key in keys {
        guards.push(mutex_for(key).lock_owned().await);
    }
    PathLock { _guards: guards }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn one_path_is_held_by_one_writer_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();

        let held = lock_path(&file).await;
        // The same file through a different spelling waits for the holder.
        let other_spelling = dir.path().join(".").join("a.txt");
        let waiting = tokio::time::timeout(Duration::from_millis(50), lock_path(&other_spelling));
        assert!(waiting.await.is_err());

        drop(held);
        tokio::time::timeout(Duration::from_secs(1), lock_path(&other_spelling))
            .await
            .expect("lock is released with its guard");
    }

    #[tokio::test]
    async fn different_paths_do_not_block_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let _a = lock_path(&dir.path().join("a.txt")).await;
        tokio::time::timeout(Duration::from_secs(1), lock_path(&dir.path().join("b.txt")))
            .await
            .expect("an unrelated path is free");
    }

    #[tokio::test]
    async fn overlapping_sets_lock_in_a_fixed_order() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        let (a2, b2) = (a.clone(), b.clone());
        let first = tokio::spawn(async move {
            for _ in 0..50 {
                let _held = lock_paths([&a2, &b2]).await;
                tokio::task::yield_now().await;
            }
        });
        let second = tokio::spawn(async move {
            for _ in 0..50 {
                let _held = lock_paths([&b, &a]).await;
                tokio::task::yield_now().await;
            }
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            first.await.unwrap();
            second.await.unwrap();
        })
        .await
        .expect("no deadlock");
    }
}
