//! One browser per persistent profile, and no browser outliving the app.
//!
//! Chromium locks its user-data-dir (`SingletonLock`, pointing at the holder's
//! pid). A second launch on a locked dir hands its command line to the holder
//! and aborts ("ProcessSingleton … aborting now to avoid profile corruption").
//! Two ways got us there:
//!
//! - **Within the app**, every agent session and sub-agent has its own
//!   [`BrowserSessionManager`](crate::BrowserSessionManager), but profile dirs
//!   are shared. [`ProfileClaim`] keeps a process-wide set of the dirs in use, so
//!   a second opener gets [`ProfileInUse::ThisApp`] instead of Chromium's abort.
//! - **Across app runs**, a persistent browser stays open between turns, and
//!   nothing killed it when the app exited or crashed (`kill_on_drop` needs a
//!   drop; macOS does not kill children with their parent). The orphan kept the
//!   lock forever. Now every launched browser gets `SIGTERM` at process exit,
//!   and a launch reaps an orphan still holding its profile: one whose parent
//!   is no longer the app that launched it, as recorded in a `<profile>.owner`
//!   file beside the dir.

use anyhow::Result;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// Why a persistent profile could not be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileInUse {
    /// Another browser of this process has the profile open.
    ThisApp,
    /// A browser launched by another running process (pid `owner`) holds it.
    OtherApp { owner: u32 },
}

impl std::fmt::Display for ProfileInUse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ThisApp => write!(
                f,
                "the profile is already open in another browser of this app"
            ),
            Self::OtherApp { owner } => write!(
                f,
                "the profile is in use by a browser of another running process (pid {owner})"
            ),
        }
    }
}

impl std::error::Error for ProfileInUse {}

#[derive(Default)]
struct Registry {
    /// Persistent profile dirs with a live browser in this process.
    profiles: HashSet<PathBuf>,
    /// Every browser process launched by this process and not yet released.
    pids: HashSet<u32>,
}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

fn with_registry<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(Registry::default))
}

/// How long a browser gets to exit after `SIGTERM` (or after we dropped it)
/// before the launch goes ahead anyway.
const EXIT_WAIT: Duration = Duration::from_secs(5);

/// A launched browser's hold on its profile dir (if persistent) and its pid.
/// Dropping it releases both.
#[derive(Default)]
pub(crate) struct ProfileClaim {
    dir: Option<PathBuf>,
    pid: Option<u32>,
}

impl ProfileClaim {
    /// Claim a persistent profile dir for a launch: refuse while this process
    /// or another running one has it open, reap an orphaned browser holding it.
    pub(crate) async fn persistent(dir: &Path) -> Result<Self, ProfileInUse> {
        let dir = dir.to_path_buf();
        if !with_registry(|r| r.profiles.insert(dir.clone())) {
            return Err(ProfileInUse::ThisApp);
        }
        // From here on the claim releases the dir again if freeing fails.
        let claim = Self {
            dir: Some(dir),
            pid: None,
        };
        free_lock(claim.dir.as_deref().unwrap()).await?;
        Ok(claim)
    }

    /// The browser is running as `pid`: terminate it at process exit, and
    /// record this process as the owner of a persistent profile.
    pub(crate) fn launched(&mut self, pid: u32) {
        with_registry(|r| r.pids.insert(pid));
        self.pid = Some(pid);
        install_exit_hook();
        if let Some(owner) = self.dir.as_deref().and_then(owner_path) {
            let _ = std::fs::write(owner, std::process::id().to_string());
        }
    }
}

impl Drop for ProfileClaim {
    fn drop(&mut self) {
        with_registry(|r| {
            if let Some(dir) = &self.dir {
                r.profiles.remove(dir);
            }
            if let Some(pid) = self.pid {
                r.pids.remove(&pid);
            }
        });
    }
}

/// `<root>/<name>` → `<root>/<name>.owner`, beside the profile like its
/// `.meta.json`, so Chromium's dir stays its own.
fn owner_path(dir: &Path) -> Option<PathBuf> {
    let name = dir.file_name()?.to_str()?;
    Some(dir.with_file_name(format!("{name}.owner")))
}

fn read_owner(dir: &Path) -> Option<u32> {
    std::fs::read_to_string(owner_path(dir)?)
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The pid in a profile's `SingletonLock` (a symlink to `<host>-<pid>`).
fn lock_holder(dir: &Path) -> Option<u32> {
    let target = std::fs::read_link(dir.join("SingletonLock")).ok()?;
    let (_, pid) = target.to_str()?.rsplit_once('-')?;
    pid.parse().ok()
}

/// A process as `ps` reports it.
#[derive(Debug, PartialEq)]
struct ProcessInfo {
    ppid: u32,
    zombie: bool,
    command: String,
}

impl ProcessInfo {
    /// `ps -o ppid=,stat=,command=` output, e.g. `  1 S  /path/Chrome --flag`.
    fn parse(line: &str) -> Option<Self> {
        let (ppid, rest) = line.trim_start().split_once(char::is_whitespace)?;
        let rest = rest.trim_start();
        let (stat, command) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        Some(Self {
            ppid: ppid.parse().ok()?,
            zombie: stat.starts_with('Z'),
            command: command.trim().to_string(),
        })
    }

    /// The process is a browser on exactly `dir` (not on a dir that merely
    /// starts with the same path, like `shop` vs. `shop-admin`).
    fn uses_profile(&self, dir: &Path) -> bool {
        let flag = format!("--user-data-dir={}", dir.display());
        self.command.match_indices(&flag).any(|(at, _)| {
            self.command[at + flag.len()..]
                .chars()
                .next()
                .is_none_or(char::is_whitespace)
        })
    }
}

/// Whether the holder of a profile is an orphan we may reap: its parent is
/// launchd/init, or no longer the process recorded as having launched it.
/// Without an owner record only the former counts, so a browser of a running
/// older app version is left alone.
fn is_orphan(ppid: u32, owner: Option<u32>) -> bool {
    ppid == 1 || owner.is_some_and(|owner| owner != ppid)
}

#[cfg(unix)]
async fn process_info(pid: u32) -> Option<ProcessInfo> {
    let out = tokio::process::Command::new("ps")
        .args(["-o", "ppid=,stat=,command=", "-p", &pid.to_string()])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    ProcessInfo::parse(String::from_utf8_lossy(&out.stdout).lines().next()?)
}

/// Wait until `pid` is gone (or a zombie); `false` on timeout.
#[cfg(unix)]
async fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match process_info(pid).await {
            None => return true,
            Some(info) if info.zombie => return true,
            Some(_) if tokio::time::Instant::now() >= deadline => return false,
            Some(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

#[cfg(unix)]
fn signal(pid: u32, signal: libc::c_int) {
    // SAFETY: plain syscall; a stale pid only yields ESRCH.
    unsafe {
        libc::kill(pid as libc::pid_t, signal);
    }
}

/// Make sure no live browser holds `dir`'s lock, or say who does.
#[cfg(unix)]
async fn free_lock(dir: &Path) -> Result<(), ProfileInUse> {
    let Some(pid) = lock_holder(dir) else {
        return Ok(());
    };
    // A dead holder or a reused pid: a stale lock Chromium clears itself.
    let Some(holder) = process_info(pid).await.filter(|h| h.uses_profile(dir)) else {
        return Ok(());
    };
    if holder.ppid == std::process::id() {
        // Our own browser, dropped without a close and still on its way out.
        wait_for_exit(pid, EXIT_WAIT).await;
        return Ok(());
    }
    if !is_orphan(holder.ppid, read_owner(dir)) {
        return Err(ProfileInUse::OtherApp { owner: holder.ppid });
    }
    tracing::info!(
        "Terminating orphaned browser (pid {pid}) holding profile {}",
        dir.display()
    );
    // SIGTERM lets Chromium shut down cleanly and flush the profile's cookies.
    signal(pid, libc::SIGTERM);
    if !wait_for_exit(pid, EXIT_WAIT).await {
        signal(pid, libc::SIGKILL);
        wait_for_exit(pid, EXIT_WAIT).await;
    }
    Ok(())
}

#[cfg(not(unix))]
async fn free_lock(_dir: &Path) -> Result<(), ProfileInUse> {
    Ok(())
}

/// Terminate every browser still running when the process exits normally
/// (`exit`, returning from `main`, macOS app termination). A crash skips this;
/// the next launch then reaps the orphan.
fn install_exit_hook() {
    #[cfg(unix)]
    {
        static INSTALLED: std::sync::Once = std::sync::Once::new();
        INSTALLED.call_once(|| {
            extern "C" fn terminate_browsers() {
                // `try_lock`: never block exit on a thread that held the lock.
                if let Ok(guard) = REGISTRY.try_lock() {
                    for pid in guard.iter().flat_map(|r| r.pids.iter()) {
                        signal(*pid, libc::SIGTERM);
                    }
                }
            }
            // SAFETY: registering a plain `extern "C"` function.
            unsafe {
                libc::atexit(terminate_browsers);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ps_output() {
        let info = ProcessInfo::parse(
            "    1 S    /Applications/Chrome --headless --user-data-dir=/p/a b --x",
        )
        .unwrap();
        assert_eq!(info.ppid, 1);
        assert!(!info.zombie);
        assert!(info.command.starts_with("/Applications/Chrome"));
        assert!(ProcessInfo::parse("  42 Z+ ").unwrap().zombie);
        assert!(ProcessInfo::parse("").is_none());
    }

    #[test]
    fn matches_the_exact_profile_dir_only() {
        let info = ProcessInfo {
            ppid: 1,
            zombie: false,
            command: "chrome --user-data-dir=/root/shop-admin --headless".into(),
        };
        assert!(info.uses_profile(Path::new("/root/shop-admin")));
        assert!(!info.uses_profile(Path::new("/root/shop")));

        // Paths with spaces (macOS Application Support), at the end of the line.
        let spaced = ProcessInfo {
            command: "chrome --user-data-dir=/Library/Application Support/p".into(),
            ..info
        };
        assert!(spaced.uses_profile(Path::new("/Library/Application Support/p")));
    }

    #[test]
    fn orphan_rules() {
        assert!(is_orphan(1, None), "reparented to launchd/init");
        assert!(is_orphan(1, Some(500)));
        assert!(is_orphan(777, Some(500)), "reparented to a subreaper");
        assert!(!is_orphan(500, Some(500)), "its launcher is alive");
        assert!(!is_orphan(777, None), "unknown launcher: leave it alone");
    }

    #[test]
    fn owner_file_sits_beside_the_profile() {
        assert_eq!(
            owner_path(Path::new("/root/browser-profiles/shop")).unwrap(),
            Path::new("/root/browser-profiles/shop.owner")
        );
    }

    #[test]
    fn reads_the_lock_holder_pid() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(lock_holder(dir.path()), None);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("my-host.local-4242", dir.path().join("SingletonLock"))
                .unwrap();
            assert_eq!(lock_holder(dir.path()), Some(4242));
        }
    }

    #[tokio::test]
    async fn a_profile_is_claimed_once_per_process() {
        let dir = tempfile::tempdir().unwrap();
        let first = ProfileClaim::persistent(dir.path()).await.unwrap();
        assert_eq!(
            ProfileClaim::persistent(dir.path()).await.err(),
            Some(ProfileInUse::ThisApp)
        );
        drop(first);
        assert!(ProfileClaim::persistent(dir.path()).await.is_ok());
    }
}
