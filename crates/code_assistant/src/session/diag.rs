//! Per-session diagnostic log for debugging execute_command hangs.
//!
//! Appends timestamped, thread-tagged lines to
//! `{data_dir}/code-assistant/sessions/{session_id}.diag.log` — right next to
//! the persisted session JSON. Intended for tracing the agent → GPUI terminal
//! path so a hung tool call leaves a ground-truth audit trail on disk that
//! survives an app restart.
//!
//! Note on scope: this is deliberately kept trivial (open-append-close per
//! line, single global write lock) because it only fires along the
//! execute_command path, not on hot code. Swap in a cached writer if that
//! ever becomes a problem.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{SecondsFormat, Utc};

fn sessions_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("code-assistant")
        .join("sessions")
}

fn log_path(session_id: &str) -> PathBuf {
    sessions_dir().join(format!("{session_id}.diag.log"))
}

static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Append a timestamped, thread-tagged line to the session's diag log.
///
/// Failures are swallowed — a broken log must not break the agent.
pub fn log(session_id: &str, msg: impl std::fmt::Display) {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let path = log_path(session_id);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let thread = std::thread::current();
    let tid = thread.id();
    let tname = thread.name().unwrap_or("-");
    let ts = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);

    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{ts} [{tid:?} {tname}] {msg}");
    }
}

/// Count currently-open file descriptors in this process by listing `/dev/fd`.
/// Returns `None` on platforms where that doesn't work.
pub fn open_fd_count() -> Option<usize> {
    #[cfg(unix)]
    {
        // On macOS /dev/fd is an fdesc mount; on Linux it's a symlink to
        // /proc/self/fd. Either way, read_dir yields one entry per open FD
        // (plus the FD opened to read the dir itself — close enough for
        // trend-spotting).
        std::fs::read_dir("/dev/fd").ok().map(|rd| rd.count())
    }

    #[cfg(not(unix))]
    {
        None
    }
}

/// A compact snapshot of resource usage, intended for embedding in diag
/// lines at entry/exit of interesting scopes.
pub fn resource_snapshot(
    terminal_pool_active: usize,
    terminal_pool_total_spawned: usize,
) -> String {
    match open_fd_count() {
        Some(fds) => format!(
            "pool_active={terminal_pool_active} pool_total={terminal_pool_total_spawned} open_fds={fds}"
        ),
        None => format!(
            "pool_active={terminal_pool_active} pool_total={terminal_pool_total_spawned} open_fds=?"
        ),
    }
}
