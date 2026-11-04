//! The environment child processes start with.
//!
//! An app started from the Finder, the Dock or a desktop launcher inherits a
//! minimal environment: on macOS `PATH` is `/usr/bin:/bin:/usr/sbin:/sbin`,
//! so tools a terminal finds (`gh`, `uvx`, `npx`, Homebrew's `git`) are
//! missing, and so is everything else the user's shell profile sets. Every
//! child process therefore starts through this crate: [`command`] and
//! [`std_command`] give it the user's login environment, captured once from
//! `$SHELL -l -i` and set on top of the app's own, and look its program up
//! on that `PATH`.
//!
//! The capture is skipped when a shell started the app (it has set the
//! environment up already), on Windows (apps get the user's environment
//! from the registry there) and when `CODE_ASSISTANT_LOGIN_ENV=0`. When it
//! fails or times out, processes get the app's own environment, as before.
//!
//! `clippy.toml` disallows `std::process::Command::new` and
//! `tokio::process::Command::new` everywhere else, so new code cannot start
//! a process past it.

#![allow(clippy::disallowed_methods)]

#[cfg(unix)]
mod login_shell;

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::OnceLock;

/// Set to `0` to keep the app's own environment for child processes.
pub const OPT_OUT_VAR: &str = "CODE_ASSISTANT_LOGIN_ENV";

static ENVIRONMENT: OnceLock<Environment> = OnceLock::new();

/// The variables child processes get on top of the app's own environment.
#[derive(Debug, Clone, Default)]
pub struct Environment {
    vars: Vec<(OsString, OsString)>,
}

/// Start capturing the login environment on a thread of its own, so the
/// first process does not wait for the shell. Call it early in `main`.
pub fn preload() {
    if ENVIRONMENT.get().is_none() {
        let _ = std::thread::Builder::new()
            .name("login-env".into())
            .spawn(|| {
                environment();
            });
    }
}

/// The environment for child processes; captured on first use (waiting for
/// a [`preload`] already under way).
pub fn environment() -> &'static Environment {
    ENVIRONMENT.get_or_init(Environment::capture)
}

/// A `std` command for `program`, in the [`environment`].
pub fn std_command(program: impl AsRef<OsStr>) -> std::process::Command {
    environment().std_command(program)
}

/// A tokio command for `program`, in the [`environment`].
pub fn command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    environment().command(program)
}

/// Where `program` is on the [`environment`]'s `PATH`.
pub fn which(program: impl AsRef<OsStr>) -> Option<PathBuf> {
    environment().which(program)
}

impl Environment {
    /// An environment that sets `vars` on top of the app's own.
    pub fn from_vars(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        Self {
            vars: vars.into_iter().collect(),
        }
    }

    fn capture() -> Self {
        if !should_capture() {
            return Self::default();
        }
        #[cfg(unix)]
        {
            let started = std::time::Instant::now();
            match login_shell::capture() {
                Ok(vars) => {
                    tracing::info!(
                        "Child processes get the login shell's environment ({} variables, {} ms)",
                        vars.len(),
                        started.elapsed().as_millis()
                    );
                    return Self::from_vars(vars);
                }
                Err(e) => {
                    tracing::warn!("Child processes keep the app's environment: {e}");
                }
            }
        }
        Self::default()
    }

    /// The variables set on top of the app's own environment.
    pub fn vars(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.vars
            .iter()
            .map(|(k, v)| (k.as_os_str(), v.as_os_str()))
    }

    /// The value child processes see for `name`.
    pub fn var(&self, name: impl AsRef<OsStr>) -> Option<OsString> {
        let name = name.as_ref();
        self.vars
            .iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .or_else(|| std::env::var_os(name))
    }

    /// Set the variables on `cmd`; ones set on it afterwards still win.
    pub fn apply(&self, cmd: &mut std::process::Command) {
        cmd.envs(self.vars());
    }

    /// A `std` command for `program`, found on this environment's `PATH`.
    pub fn std_command(&self, program: impl AsRef<OsStr>) -> std::process::Command {
        // `Command` searches the `PATH` it was given for a bare program name.
        let mut cmd = std::process::Command::new(program);
        self.apply(&mut cmd);
        cmd
    }

    /// A tokio command for `program`, found on this environment's `PATH`.
    pub fn command(&self, program: impl AsRef<OsStr>) -> tokio::process::Command {
        self.std_command(program).into()
    }

    /// Where `program` is on this environment's `PATH`.
    pub fn which(&self, program: impl AsRef<OsStr>) -> Option<PathBuf> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        which::which_in(program, self.var("PATH"), cwd).ok()
    }
}

fn should_capture() -> bool {
    if cfg!(windows) || std::env::var_os(OPT_OUT_VAR).is_some_and(|v| v == "0") {
        return false;
    }
    // A shell sets SHLVL; the app was started from a terminal (or by a
    // program a shell started) and has the user's environment already.
    std::env::var_os("SHLVL").is_none()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn executable(dir: &std::path::Path, name: &str, script: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn path_with(dir: &std::path::Path) -> OsString {
        let mut path = dir.as_os_str().to_owned();
        path.push(":/usr/bin:/bin");
        path
    }

    #[test]
    fn a_program_is_found_on_the_environments_path() {
        let dir = tempfile::tempdir().unwrap();
        let tool = executable(dir.path(), "only-here", "echo found");
        let env = Environment::from_vars([("PATH".into(), path_with(dir.path()))]);

        assert_eq!(env.which("only-here"), Some(tool));
        let output = env.std_command("only-here").output().unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout), "found\n");
        assert!(Environment::default().which("only-here").is_none());
    }

    #[tokio::test]
    async fn variables_set_on_the_command_win_over_the_environments() {
        let env = Environment::from_vars([
            ("CA_TEST_LOGIN".into(), "login".into()),
            ("CA_TEST_BOTH".into(), "login".into()),
        ]);
        let output = env
            .command("sh")
            .args(["-c", "echo $CA_TEST_LOGIN $CA_TEST_BOTH"])
            .env("CA_TEST_BOTH", "server")
            .output()
            .await
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout), "login server\n");
    }

    #[test]
    fn a_variable_the_login_shell_lacks_comes_from_the_app() {
        let env = Environment::from_vars([("CA_TEST_ONLY_LOGIN".into(), "x".into())]);
        assert_eq!(env.var("CA_TEST_ONLY_LOGIN"), Some("x".into()));
        assert_eq!(env.var("HOME"), std::env::var_os("HOME"));
    }
}
