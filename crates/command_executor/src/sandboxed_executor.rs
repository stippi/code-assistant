use std::path::PathBuf;

use anyhow::{bail, Result};
use async_trait::async_trait;
use sandbox::SandboxPolicy;
#[cfg(not(target_os = "macos"))]
use tracing::warn;

use crate::{CommandExecutor, CommandOutput, StreamingCallback};

#[cfg(target_os = "macos")]
use {
    sandbox::SeatbeltInvocation,
    std::process::Stdio,
    tokio::io::{AsyncBufReadExt, BufReader},
    tokio::process::Command as TokioCommand,
};

/// Wraps a command executor with sandbox policy metadata. Actual enforcement will be
/// introduced per-platform; for now this records intent and keeps the policy accessible.
pub struct SandboxedCommandExecutor {
    inner: Box<dyn CommandExecutor>,
    policy: SandboxPolicy,
    #[allow(dead_code)]
    session_id: Option<String>,
}

impl SandboxedCommandExecutor {
    pub fn new(
        inner: Box<dyn CommandExecutor>,
        policy: SandboxPolicy,
        session_id: Option<String>,
    ) -> Self {
        Self {
            inner,
            policy,
            session_id,
        }
    }

    #[allow(dead_code)]
    pub fn policy(&self) -> &SandboxPolicy {
        &self.policy
    }
}

#[async_trait]
impl CommandExecutor for SandboxedCommandExecutor {
    async fn execute(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
    ) -> Result<CommandOutput> {
        if !self.policy.requires_restrictions() {
            return self.inner.execute(command_line, working_dir).await;
        }

        #[cfg(target_os = "macos")]
        {
            return self
                .execute_with_seatbelt(command_line, working_dir, true, None)
                .await;
        }

        #[cfg(not(target_os = "macos"))]
        {
            warn!(
                "Sandbox policy {:?} requested but sandboxing is not supported on this platform; running unrestricted",
                self.policy
            );
            self.inner.execute(command_line, working_dir).await
        }
    }

    async fn execute_streaming(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
        callback: Option<&dyn StreamingCallback>,
    ) -> Result<CommandOutput> {
        if !self.policy.requires_restrictions() {
            return self
                .inner
                .execute_streaming(command_line, working_dir, callback)
                .await;
        }

        #[cfg(target_os = "macos")]
        {
            return self
                .execute_with_seatbelt(command_line, working_dir, false, callback)
                .await;
        }

        #[cfg(not(target_os = "macos"))]
        {
            warn!(
                "Sandbox policy {:?} requested but sandboxing is not supported on this platform; running unrestricted",
                self.policy
            );
            self.inner
                .execute_streaming(command_line, working_dir, callback)
                .await
        }
    }
}

#[cfg(target_os = "macos")]
impl SandboxedCommandExecutor {
    async fn execute_with_seatbelt(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
        redirect_stderr: bool,
        callback: Option<&dyn StreamingCallback>,
    ) -> Result<CommandOutput> {
        let cwd = canonical_working_dir(working_dir)?;
        let (shell, shell_args) = shell_command(command_line, redirect_stderr);
        let mut command_vec = Vec::with_capacity(shell_args.len() + 1);
        command_vec.push(shell);
        command_vec.extend(shell_args);

        let invocation = sandbox::build_seatbelt_invocation(command_vec, &self.policy, &cwd)
            .map_err(|e| anyhow::anyhow!("Failed to prepare seatbelt invocation: {e}"))?;

        if redirect_stderr {
            self.run_blocking(invocation, &cwd).await
        } else {
            self.run_streaming(invocation, &cwd, callback).await
        }
    }

    async fn run_blocking(
        &self,
        invocation: SeatbeltInvocation,
        cwd: &PathBuf,
    ) -> Result<CommandOutput> {
        let SeatbeltInvocation {
            executable,
            args,
            policy_path,
        } = invocation;

        let mut cmd = std::process::Command::new(executable);
        cmd.args(&args);
        cmd.current_dir(cwd);
        cmd.env("CODE_ASSISTANT_SANDBOX", "seatbelt");
        if !self.policy.has_full_network_access() {
            cmd.env("CODE_ASSISTANT_SANDBOX_NETWORK_DISABLED", "1");
        }

        let output = cmd.output()?;
        drop(policy_path);

        let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
        if !output.stderr.is_empty() {
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
        }

        Ok(CommandOutput {
            success: output.status.success(),
            output: combined,
        })
    }

    async fn run_streaming(
        &self,
        invocation: SeatbeltInvocation,
        cwd: &PathBuf,
        callback: Option<&dyn StreamingCallback>,
    ) -> Result<CommandOutput> {
        let SeatbeltInvocation {
            executable,
            args,
            policy_path,
        } = invocation;

        let mut cmd = TokioCommand::new(executable);
        cmd.args(&args);
        cmd.current_dir(cwd);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(Stdio::null());
        cmd.env("CODE_ASSISTANT_SANDBOX", "seatbelt");
        if !self.policy.has_full_network_access() {
            cmd.env("CODE_ASSISTANT_SANDBOX_NETWORK_DISABLED", "1");
        }

        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let mut accumulated_output = String::new();

        if let Some(callback) = callback {
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
            let output = child.wait_with_output().await?;
            accumulated_output = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr_output = String::from_utf8_lossy(&output.stderr);
            if !stderr_output.is_empty() {
                accumulated_output.push_str(&stderr_output);
            }
            drop(policy_path);
            return Ok(CommandOutput {
                success: output.status.success(),
                output: accumulated_output,
            });
        }

        let status = child.wait().await?;
        drop(policy_path);

        Ok(CommandOutput {
            success: status.success(),
            output: accumulated_output,
        })
    }
}

fn shell_command(command_line: &str, redirect_stderr: bool) -> (String, Vec<String>) {
    #[cfg(target_family = "unix")]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let mut args = Vec::new();
        args.push("-c".to_string());
        if redirect_stderr {
            args.push(format!("{command_line} 2>&1"));
        } else {
            args.push(command_line.to_string());
        }
        (shell, args)
    }

    #[cfg(target_family = "windows")]
    {
        let shell = "cmd".to_string();
        let mut args = Vec::new();
        args.push("/C".to_string());
        if redirect_stderr {
            args.push(format!("{command_line} 2>&1"));
        } else {
            args.push(command_line.to_string());
        }
        (shell, args)
    }
}

fn canonical_working_dir(working_dir: Option<&PathBuf>) -> Result<PathBuf> {
    match working_dir {
        Some(dir) => {
            if !dir.exists() {
                bail!("Working directory does not exist: {}", dir.display());
            }
            if !dir.is_dir() {
                bail!("Path is not a directory: {}", dir.display());
            }
            Ok(dir.canonicalize().unwrap_or_else(|_| dir.clone()))
        }
        None => Ok(std::env::current_dir()?),
    }
}
