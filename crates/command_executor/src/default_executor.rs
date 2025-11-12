use crate::{CommandExecutor, CommandOutput, SandboxCommandRequest, StreamingCallback};
use anyhow::Result;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

pub struct DefaultCommandExecutor;

#[async_trait::async_trait]
impl CommandExecutor for DefaultCommandExecutor {
    async fn execute(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
        _sandbox_request: Option<&SandboxCommandRequest>,
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
        _sandbox_request: Option<&SandboxCommandRequest>,
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
