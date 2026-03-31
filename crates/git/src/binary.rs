use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Wrapper around the system `git` binary.
///
/// Handles locating the binary and building commands with the correct
/// working directory and safety flags.
#[derive(Debug, Clone)]
pub(crate) struct GitBinary {
    binary_path: PathBuf,
}

impl GitBinary {
    /// Locate the system git binary.
    pub fn new() -> Result<Self> {
        let binary_path = which::which("git").context("git binary not found in PATH")?;
        Ok(Self { binary_path })
    }

    /// Build a `Command` that runs git in the given working directory.
    pub fn command(&self, working_dir: &Path) -> Command {
        let mut cmd = Command::new(&self.binary_path);
        cmd.current_dir(working_dir);
        // Disable fsmonitor for predictable behavior
        cmd.args(["-c", "core.fsmonitor=false"]);
        cmd.arg("--no-pager");
        // Prevent git from prompting for input
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        cmd
    }

    /// Run a git command and return stdout on success.
    pub async fn run<S: AsRef<OsStr>>(&self, working_dir: &Path, args: &[S]) -> Result<String> {
        let mut cmd = self.command(working_dir);
        cmd.args(args);

        let output = cmd
            .output()
            .await
            .context("Failed to execute git command")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let args_display: Vec<_> = args
                .iter()
                .map(|a| a.as_ref().to_string_lossy().to_string())
                .collect();
            bail!(
                "git {} failed (exit {}): {}",
                args_display.join(" "),
                output.status.code().unwrap_or(-1),
                stderr.trim()
            );
        }

        let mut stdout =
            String::from_utf8(output.stdout).context("git output is not valid UTF-8")?;

        // Strip trailing newline, like Zed does
        if stdout.ends_with('\n') {
            stdout.pop();
            if stdout.ends_with('\r') {
                stdout.pop();
            }
        }

        Ok(stdout)
    }
}
