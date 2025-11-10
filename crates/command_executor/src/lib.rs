use anyhow::Result;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

#[derive(Clone)]
pub struct CommandOutput {
    pub success: bool,
    pub output: String,
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
    ) -> Result<CommandOutput>;

    /// Execute command with streaming output callback
    async fn execute_streaming(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
        callback: Option<&dyn StreamingCallback>,
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

pub struct DefaultCommandExecutor;

#[async_trait::async_trait]
impl CommandExecutor for DefaultCommandExecutor {
    async fn execute(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
    ) -> Result<CommandOutput> {
        // Validate working_dir first
        if let Some(dir) = working_dir {
            if !dir.exists() {
                return Err(anyhow::anyhow!(
                    "Working directory does not exist: {}",
                    dir.display()
                ));
            }
            if !dir.is_dir() {
                return Err(anyhow::anyhow!(
                    "Path is not a directory: {}",
                    dir.display()
                ));
            }
        }

        // Create shell command using login shell or fallback
        #[cfg(target_family = "unix")]
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        #[cfg(target_family = "unix")]
        let mut cmd = std::process::Command::new(shell);
        #[cfg(target_family = "unix")]
        cmd.args(["-c", &format!("{command_line} 2>&1")]);

        #[cfg(target_family = "windows")]
        let mut cmd = std::process::Command::new("cmd");
        #[cfg(target_family = "windows")]
        cmd.args(["/C", &format!("{} 2>&1", command_line)]);

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }
        let output = cmd.output()?;

        Ok(CommandOutput {
            success: output.status.success(),
            output: String::from_utf8_lossy(&output.stdout).into_owned(),
        })
    }

    async fn execute_streaming(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
        callback: Option<&dyn StreamingCallback>,
    ) -> Result<CommandOutput> {
        // Validate working_dir first
        if let Some(dir) = working_dir {
            if !dir.exists() {
                return Err(anyhow::anyhow!(
                    "Working directory does not exist: {}",
                    dir.display()
                ));
            }
            if !dir.is_dir() {
                return Err(anyhow::anyhow!(
                    "Path is not a directory: {}",
                    dir.display()
                ));
            }
        }

        // Create shell command using login shell or fallback
        #[cfg(target_family = "unix")]
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        #[cfg(target_family = "unix")]
        let mut cmd = Command::new(shell);
        #[cfg(target_family = "unix")]
        cmd.args(["-c", command_line]);

        #[cfg(target_family = "windows")]
        let mut cmd = Command::new("cmd");
        #[cfg(target_family = "windows")]
        cmd.args(["/C", command_line]);

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        // Configure to capture stdout and stderr separately for streaming
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn()?;

        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let mut accumulated_output = String::new();

        if let Some(callback) = callback {
            // Stream stdout and stderr concurrently
            let stdout_reader = BufReader::new(stdout);
            let stderr_reader = BufReader::new(stderr);

            let mut stdout_lines = stdout_reader.lines();
            let mut stderr_lines = stderr_reader.lines();

            let mut stdout_done = false;
            let mut stderr_done = false;

            while !stdout_done || !stderr_done {
                tokio::select! {
                    line = stdout_lines.next_line(), if !stdout_done => {
                        match line? {
                            Some(line) => {
                                let line_with_newline = format!("{line}\n");
                                accumulated_output.push_str(&line_with_newline);
                                let _ = callback.on_output_chunk(&line_with_newline);
                            }
                            None => stdout_done = true,
                        }
                    }
                    line = stderr_lines.next_line(), if !stderr_done => {
                        match line? {
                            Some(line) => {
                                let line_with_newline = format!("{line}\n");
                                accumulated_output.push_str(&line_with_newline);
                                let _ = callback.on_output_chunk(&line_with_newline);
                            }
                            None => stderr_done = true,
                        }
                    }
                }
            }
        } else {
            // No callback, just wait for completion and read all output
            let output = child.wait_with_output().await?;
            accumulated_output = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr_output = String::from_utf8_lossy(&output.stderr);
            if !stderr_output.is_empty() {
                accumulated_output.push_str(&stderr_output);
            }

            return Ok(CommandOutput {
                success: output.status.success(),
                output: accumulated_output,
            });
        }

        // Wait for the process to complete
        let status = child.wait().await?;

        Ok(CommandOutput {
            success: status.success(),
            output: accumulated_output,
        })
    }
}
