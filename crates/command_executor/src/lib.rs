use anyhow::Result;
use std::path::{Path, PathBuf};

mod default_executor;
mod sandboxed_executor;
pub use default_executor::DefaultCommandExecutor;
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

    fn on_terminal_attached(&self, _terminal_id: &str) -> Result<()> {
        Ok(())
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
