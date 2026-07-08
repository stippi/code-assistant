use anyhow::Result;
use std::path::{Path, PathBuf};

mod default_executor;
mod pty_executor;
mod sandboxed_executor;
pub use default_executor::DefaultCommandExecutor;
pub use pty_executor::PtyCommandExecutor;
pub use sandboxed_executor::SandboxedCommandExecutor;

#[derive(Clone)]
pub struct CommandOutput {
    pub success: bool,
    pub output: String,
}

#[derive(Clone, Debug, Default)]
pub struct SandboxCommandRequest {
    pub writable_roots: Vec<PathBuf>,
    pub read_only: bool,
    pub bypass_sandbox: bool,
}

/// Callback trait for streaming command output
pub trait StreamingCallback: Send + Sync {
    fn on_output_chunk(&self, chunk: &str) -> Result<()>;

    /// Raw terminal output (ANSI escape sequences included), for frontends
    /// that render it in a terminal emulator. Plain-text consumers keep
    /// using `on_output_chunk` and can ignore this.
    fn on_terminal_output_chunk(&self, _bytes: &[u8]) -> Result<()> {
        Ok(())
    }

    fn on_terminal_attached(&self, _terminal_id: &str) -> Result<()> {
        Ok(())
    }

    /// The process exited (`exit_code` is `None` when no code is known).
    /// Lets a terminal-emulator frontend mark its display-only terminal
    /// finished; plain-text consumers can ignore it.
    fn on_terminal_exit(&self, _exit_code: Option<i32>) -> Result<()> {
        Ok(())
    }

    /// Returns the tool invocation ID associated with this callback, if any.
    fn tool_id(&self) -> Option<&str> {
        None
    }

    /// Whether the command should keep running. A streaming executor polls
    /// this between output windows; returning `false` makes it interrupt the
    /// process (used by the UI's terminal-card stop button). Default `true`.
    fn should_continue(&self) -> bool {
        true
    }
}

/// A prepared spawn for a long-lived interactive (PTY) session: the argv to
/// run, extra environment, and an opaque guard the spawned session must keep
/// alive (e.g. the temp file holding a sandbox profile the argv references).
pub struct PtySpawnSpec {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub keep_alive: Option<Box<dyn std::any::Any + Send>>,
}

impl PtySpawnSpec {
    /// Plain, unsandboxed "run through the user's shell" spawn.
    pub fn shell(command_line: &str) -> Self {
        #[cfg(target_family = "unix")]
        let argv = vec![
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string()),
            "-c".to_string(),
            command_line.to_string(),
        ];
        #[cfg(target_family = "windows")]
        let argv = vec![
            "cmd".to_string(),
            "/C".to_string(),
            command_line.to_string(),
        ];
        Self {
            argv,
            env: Vec::new(),
            keep_alive: None,
        }
    }
}

#[async_trait::async_trait]
pub trait CommandExecutor: Send + Sync {
    async fn execute(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
        sandbox_request: Option<&SandboxCommandRequest>,
    ) -> Result<CommandOutput>;

    /// Execute command with streaming output callback
    async fn execute_streaming(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
        callback: Option<&dyn StreamingCallback>,
        sandbox_request: Option<&SandboxCommandRequest>,
    ) -> Result<CommandOutput>;

    /// Prepare the argv (and env/guard) for spawning `command_line` as a
    /// long-lived interactive session, applying the executor's sandbox
    /// wrapping. The caller spawns the process itself — sessions outlive a
    /// single `execute` call, so they can't run through the executor.
    fn prepare_pty_spawn(
        &self,
        command_line: &str,
        _working_dir: &Path,
        _sandbox_request: Option<&SandboxCommandRequest>,
    ) -> Result<PtySpawnSpec> {
        Ok(PtySpawnSpec::shell(command_line))
    }
}

/// Quote a path for the current platform so spaces and special chars are preserved when passed
/// through the shell. This is a best-effort helper; it does not aim to be a full shell-quoting lib.
pub fn shell_quote_path(path: &Path) -> String {
    #[cfg(target_family = "unix")]
    {
        let s = path.to_string_lossy();
        // Only quote if whitespace is present; basic behavior for tests
        if s.chars().any(|c| c.is_whitespace()) {
            let escaped = s.replace('\'', "'\\''");
            format!("'{escaped}'")
        } else {
            s.to_string()
        }
    }

    #[cfg(target_family = "windows")]
    {
        let s = path.to_string_lossy();
        if s.chars().any(|c| c.is_whitespace()) {
            // Surround with double quotes and escape internal quotes by doubling them
            let escaped = s.replace('"', "\"\"");
            format!("\"{escaped}\"")
        } else {
            s.to_string()
        }
    }
}

/// Build a formatter command line from a template. If the template contains the {path} placeholder,
/// it will be replaced with the (quoted) relative path. If not present, the template is returned as-is.
pub fn build_format_command(template: &str, relative_path: &Path) -> String {
    if template.contains("{path}") {
        let quoted = shell_quote_path(relative_path);
        template.replace("{path}", &quoted)
    } else {
        template.to_string()
    }
}
