use agent_client_protocol::schema as acp;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::time::{Duration, Instant};

use crate::ClientConn;
use command_executor::{CommandExecutor, CommandOutput, SandboxCommandRequest, StreamingCallback};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
const OUTPUT_BYTE_LIMIT: u64 = 1_048_576;
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// CommandExecutor implementation that uses the ACP Terminal Protocol instead of
/// executing commands locally.
///
/// The SDK connection is `Send + Clone`, so the executor holds a
/// `ConnectionTo<Client>` and drives the `terminal/*` RPCs directly from the
/// agent task (no `spawn_local` worker needed).
pub struct ACPTerminalCommandExecutor {
    session_id: acp::SessionId,
    conn: ClientConn,
    default_timeout: Duration,
}

impl ACPTerminalCommandExecutor {
    pub fn new(session_id: acp::SessionId, conn: ClientConn) -> Self {
        Self {
            session_id,
            conn,
            default_timeout: DEFAULT_TIMEOUT,
        }
    }
}

#[async_trait]
impl CommandExecutor for ACPTerminalCommandExecutor {
    async fn execute(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
        sandbox_request: Option<&SandboxCommandRequest>,
    ) -> Result<CommandOutput> {
        self.execute_streaming(command_line, working_dir, None, sandbox_request)
            .await
    }

    async fn execute_streaming(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
        callback: Option<&dyn StreamingCallback>,
        _sandbox_request: Option<&SandboxCommandRequest>,
    ) -> Result<CommandOutput> {
        self.run_command(command_line, working_dir.cloned(), callback)
            .await
    }
}

impl ACPTerminalCommandExecutor {
    async fn run_command(
        &self,
        command_line: &str,
        cwd: Option<PathBuf>,
        callback: Option<&dyn StreamingCallback>,
    ) -> Result<CommandOutput> {
        // Pass the complete command line as the command parameter with empty args.
        // This avoids escaping issues on the Zed side when args are passed separately.
        let create_request =
            acp::CreateTerminalRequest::new(self.session_id.clone(), command_line.to_string())
                .cwd(cwd)
                .output_byte_limit(OUTPUT_BYTE_LIMIT);

        let create_response = self
            .conn
            .send_request(create_request)
            .block_task()
            .await
            .map_err(|e| anyhow!("Failed to create terminal: {e}"))?;

        let terminal_id = create_response.terminal_id;

        if let Some(cb) = callback {
            cb.on_terminal_attached(terminal_id.0.as_ref())?;
        }

        let result = if callback.is_some() {
            self.stream_terminal_output(&terminal_id, self.default_timeout, callback)
                .await
        } else {
            self.wait_for_terminal_completion(&terminal_id, self.default_timeout)
                .await
        };

        let release_request =
            acp::ReleaseTerminalRequest::new(self.session_id.clone(), terminal_id.clone());

        match (
            result,
            self.conn
                .send_request(release_request)
                .block_task()
                .await
                .map_err(|e| anyhow!("Failed to release terminal: {e}")),
        ) {
            (Ok(output), Ok(_)) => Ok(output),
            (Ok(_), Err(release_err)) => Err(release_err),
            (Err(err), Ok(_)) => Err(err),
            (Err(err), Err(release_err)) => {
                tracing::warn!("Failed to release terminal after error: {release_err}");
                Err(err)
            }
        }
    }

    async fn stream_terminal_output(
        &self,
        terminal_id: &acp::TerminalId,
        timeout: Duration,
        callback: Option<&dyn StreamingCallback>,
    ) -> Result<CommandOutput> {
        let deadline = Instant::now() + timeout;
        let mut seen_len = 0usize;

        loop {
            let output_response = self
                .conn
                .send_request(acp::TerminalOutputRequest::new(
                    self.session_id.clone(),
                    terminal_id.clone(),
                ))
                .block_task()
                .await
                .map_err(|e| anyhow!("Failed to get terminal output: {e}"))?;

            let output_current = output_response.output;

            if output_current.len() < seen_len {
                // The client truncated the buffer from the front. Reset our cursor.
                seen_len = 0;
            }

            if output_current.len() > seen_len {
                let chunk = output_current[seen_len..].to_string();
                if let Some(cb) = callback {
                    cb.on_output_chunk(&chunk)?;
                }
                seen_len = output_current.len();
            }

            if output_response.truncated {
                tracing::warn!(
                    "ACP terminal output truncated for session {}",
                    self.session_id.0
                );
            }

            if let Some(status) = output_response.exit_status {
                let success = status.exit_code.map(|code| code == 0).unwrap_or(false);

                return Ok(CommandOutput {
                    success,
                    output: output_current,
                });
            }

            if Instant::now() >= deadline {
                let _ = self
                    .conn
                    .send_request(acp::KillTerminalRequest::new(
                        self.session_id.clone(),
                        terminal_id.clone(),
                    ))
                    .block_task()
                    .await
                    .map_err(|e| anyhow!("Failed to kill terminal after timeout: {e}"))?;

                return Err(anyhow!("Command timed out after {timeout:?}"));
            }

            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    async fn wait_for_terminal_completion(
        &self,
        terminal_id: &acp::TerminalId,
        timeout: Duration,
    ) -> Result<CommandOutput> {
        let wait_future = self
            .conn
            .send_request(acp::WaitForTerminalExitRequest::new(
                self.session_id.clone(),
                terminal_id.clone(),
            ))
            .block_task();

        let wait_response = tokio::time::timeout(timeout, wait_future)
            .await
            .map_err(|_| anyhow!("Command timed out after {timeout:?}"))?
            .map_err(|e| anyhow!("Failed to wait for terminal exit: {e}"))?;

        let output_response = self
            .conn
            .send_request(acp::TerminalOutputRequest::new(
                self.session_id.clone(),
                terminal_id.clone(),
            ))
            .block_task()
            .await
            .map_err(|e| anyhow!("Failed to read terminal output: {e}"))?;

        let success = output_response
            .exit_status
            .or(Some(wait_response.exit_status))
            .and_then(|status| status.exit_code)
            .map(|code| code == 0)
            .unwrap_or(false);

        Ok(CommandOutput {
            success,
            output: output_response.output,
        })
    }
}
