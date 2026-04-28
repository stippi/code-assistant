//! Filesystem utilities for safe multi-process persistence.
//!
//! Provides:
//! - **Atomic writes** via write-to-temp-then-rename, so a crash mid-write never
//!   leaves a partially-written file.
//! - **Advisory file locking** via `fs2` (`flock` on POSIX, `LockFile` on
//!   Windows) to serialise read-modify-write cycles across processes.
//! - **Per-session agent lock files** to enforce a single active agent loop per
//!   session across independent code-assistant instances.

use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use tracing::debug;

// ---------------------------------------------------------------------------
// Atomic write
// ---------------------------------------------------------------------------

/// Write `data` to `path` atomically.
///
/// Creates a temporary file in the same directory, writes the content, then
/// renames it over `path`.  On POSIX the rename is atomic; on Windows it uses
/// `MoveFileEx(REPLACE_EXISTING)` which is as close to atomic as the OS allows.
///
/// If the process crashes between creating the temp file and renaming, a stale
/// `.tmp*` file is left behind but `path` is never corrupted.
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .context("atomic_write: path has no parent directory")?;

    // Ensure the directory exists (important for first-time writes)
    fs::create_dir_all(dir)?;

    let mut tmp = tempfile::NamedTempFile::new_in(dir).with_context(|| {
        format!(
            "atomic_write: failed to create temp file in {}",
            dir.display()
        )
    })?;

    tmp.write_all(data)
        .context("atomic_write: failed to write data to temp file")?;

    // Flush to OS before rename so the data is on disk.
    tmp.as_file()
        .sync_all()
        .context("atomic_write: failed to sync temp file")?;

    tmp.persist(path).with_context(|| {
        format!(
            "atomic_write: failed to rename temp file to {}",
            path.display()
        )
    })?;

    Ok(())
}

/// Convenience wrapper: serialise `data` as pretty JSON and write atomically.
pub fn atomic_write_json<T: serde::Serialize + ?Sized>(path: &Path, data: &T) -> Result<()> {
    let json = serde_json::to_string_pretty(data)?;
    atomic_write(path, json.as_bytes())
}

// ---------------------------------------------------------------------------
// Advisory file locking for metadata operations
// ---------------------------------------------------------------------------

/// A guard that holds an exclusive advisory lock on a file.
///
/// The lock is released when this guard is dropped.  If the process crashes the
/// OS releases the lock automatically (`flock` semantics).
pub struct FileLockGuard {
    _file: File,
}

/// Acquire an exclusive advisory lock on the given lock-file path.
///
/// Creates the lock file if it doesn't exist.  Blocks until the lock is
/// acquired.
pub fn lock_exclusive(lock_path: &Path) -> Result<FileLockGuard> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)
        .with_context(|| format!("failed to open lock file {}", lock_path.display()))?;

    file.lock_exclusive().with_context(|| {
        format!(
            "failed to acquire exclusive lock on {}",
            lock_path.display()
        )
    })?;

    Ok(FileLockGuard { _file: file })
}

// ---------------------------------------------------------------------------
// Per-session agent lock
// ---------------------------------------------------------------------------

/// An RAII guard for the per-session agent lock.
///
/// While this guard exists, no other process (or the same process) can acquire
/// the agent lock for the same session.  The lock is released on drop or
/// process exit.
pub struct AgentLockGuard {
    _file: File,
    path: PathBuf,
}

impl AgentLockGuard {
    /// Path of the underlying lock file (useful for diagnostics).
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for AgentLockGuard {
    fn drop(&mut self) {
        // The flock is released when the file descriptor is closed (automatic).
        // We also try to delete the lock file for cleanliness, but this is
        // best-effort — the presence of the file alone doesn't indicate a lock.
        let _ = fs::remove_file(&self.path);
        debug!("Released agent lock: {}", self.path.display());
    }
}

/// Try to acquire the agent lock for a session.
///
/// Returns `Ok(Some(guard))` if the lock was acquired, `Ok(None)` if another
/// process already holds it.  The lock file is located at
/// `<sessions_dir>/<session_id>.agent.lock`.
pub fn try_acquire_agent_lock(
    sessions_dir: &Path,
    session_id: &str,
) -> Result<Option<AgentLockGuard>> {
    let lock_path = sessions_dir.join(format!("{session_id}.agent.lock"));

    fs::create_dir_all(sessions_dir)?;

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("failed to open agent lock file {}", lock_path.display()))?;

    // Non-blocking try-lock
    match file.try_lock_exclusive() {
        Ok(()) => {
            // Write PID for diagnostics
            let mut f = &file;
            let _ = f.write_all(format!("{}", std::process::id()).as_bytes());
            let _ = f.flush();
            debug!(
                "Acquired agent lock for session {session_id}: {}",
                lock_path.display()
            );
            Ok(Some(AgentLockGuard {
                _file: file,
                path: lock_path,
            }))
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            debug!("Agent lock already held for session {session_id}");
            Ok(None)
        }
        Err(e) => {
            // On some platforms the error kind for "already locked" may differ
            // from WouldBlock.  Check the raw OS error.
            #[cfg(unix)]
            {
                if let Some(raw) = e.raw_os_error() {
                    // EAGAIN/EWOULDBLOCK: 11 on Linux, 35 on macOS
                    if raw == 11 || raw == 35 {
                        debug!(
                            "Agent lock already held for session {session_id} (raw errno {raw})"
                        );
                        return Ok(None);
                    }
                }
            }
            Err(anyhow::anyhow!(
                "failed to try-lock agent lock file {}: {}",
                lock_path.display(),
                e
            ))
        }
    }
}

/// Check whether a session's agent lock is currently held by another process.
///
/// Returns `true` if the lock is held (i.e. we cannot acquire it),
/// `false` if it is free.
pub fn is_agent_locked(sessions_dir: &Path, session_id: &str) -> bool {
    let lock_path = sessions_dir.join(format!("{session_id}.agent.lock"));

    let Ok(file) = OpenOptions::new()
        .create(false)
        .write(true)
        .open(&lock_path)
    else {
        return false; // File doesn't exist → not locked
    };

    match file.try_lock_exclusive() {
        Ok(()) => {
            // We acquired the lock → it wasn't held.  Release immediately.
            let _ = file.unlock();
            false
        }
        Err(_) => true, // Cannot acquire → held by another process
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn atomic_write_creates_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.json");

        atomic_write(&path, b"hello").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn atomic_write_overwrites_existing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.json");

        fs::write(&path, "old").unwrap();
        atomic_write(&path, b"new").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn atomic_write_json_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("data.json");

        let data = vec!["alpha", "beta"];
        atomic_write_json(&path, &data).unwrap();

        let loaded: Vec<String> =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded, data);
    }

    #[test]
    fn lock_exclusive_and_release() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("test.lock");

        let guard = lock_exclusive(&lock_path).unwrap();
        assert!(lock_path.exists());
        drop(guard);
    }

    #[test]
    fn agent_lock_acquire_and_release() {
        let dir = tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");

        // First acquire should succeed
        let guard = try_acquire_agent_lock(&sessions_dir, "sess1").unwrap();
        assert!(guard.is_some());

        // Second acquire from same process should fail (already locked)
        let guard2 = try_acquire_agent_lock(&sessions_dir, "sess1").unwrap();
        assert!(guard2.is_none());

        // Check is_agent_locked
        assert!(is_agent_locked(&sessions_dir, "sess1"));

        // Release
        drop(guard);

        // Now should be free
        assert!(!is_agent_locked(&sessions_dir, "sess1"));
    }

    #[test]
    fn agent_lock_different_sessions_independent() {
        let dir = tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");

        let guard1 = try_acquire_agent_lock(&sessions_dir, "sess1").unwrap();
        assert!(guard1.is_some());

        let guard2 = try_acquire_agent_lock(&sessions_dir, "sess2").unwrap();
        assert!(guard2.is_some());

        drop(guard1);
        drop(guard2);
    }

    #[test]
    fn is_agent_locked_returns_false_when_no_file() {
        let dir = tempdir().unwrap();
        assert!(!is_agent_locked(dir.path(), "nonexistent"));
    }
}
