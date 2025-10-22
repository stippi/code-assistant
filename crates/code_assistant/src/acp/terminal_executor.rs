use agent_client_protocol::{self as acp, Client};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use shell_words::split as split_command_line;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::{Duration, Instant};

use crate::utils::command::{CommandExecutor, CommandOutput, StreamingCallback};
use crate::utils::DefaultCommandExecutor;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
const OUTPUT_BYTE_LIMIT: u64 = 1_048_576;
const POLL_INTERVAL: Duration = Duration::from_millis(100);

static TERMINAL_WORKER: OnceLock<UnboundedSender<TerminalWorkerRequest>> = OnceLock::new();

/// Register the background worker that proxies terminal RPC calls onto the
/// `LocalSet` that owns the ACP connection. Must be called from that
/// `LocalSet` so `spawn_local` is available.
pub fn register_terminal_worker(connection: Arc<acp::AgentSideConnection>) {
    if TERMINAL_WORKER.get().is_some() {
        tracing::warn!("ACP terminal worker already registered");
        return;
    }

    let (tx, rx) = mpsc::unbounded_channel();
    match TERMINAL_WORKER.set(tx.clone()) {
        Ok(()) => {
            tokio::task::spawn_local(async move {
                run_terminal_worker(connection, rx).await;
            });
        }
        Err(_) => {
            tracing::warn!("ACP terminal worker registration raced");
        }
    }
}

fn terminal_worker_sender() -> Option<UnboundedSender<TerminalWorkerRequest>> {
    TERMINAL_WORKER.get().cloned()
}

/// CommandExecutor implementation that uses ACP Terminal Protocol
/// instead of executing commands locally.
pub struct ACPTerminalCommandExecutor {
    session_id: acp::SessionId,
    default_timeout: Duration,
}

impl ACPTerminalCommandExecutor {
    pub fn new(session_id: acp::SessionId) -> Self {
        Self {
            session_id,
            default_timeout: DEFAULT_TIMEOUT,
        }
    }

    fn parse_command_line(command_line: &str) -> Result<(String, Vec<String>)> {
        let mut parts = split_command_line(command_line)
            .map_err(|e| anyhow!("Failed to parse command line '{command_line}': {e}"))?
            .into_iter();

        let command = parts
            .next()
            .ok_or_else(|| anyhow!("Command line is empty"))?;
        let args = parts.collect();
        Ok((command, args))
    }
}

#[async_trait]
impl CommandExecutor for ACPTerminalCommandExecutor {
    async fn execute(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
    ) -> Result<CommandOutput> {
        self.execute_streaming(command_line, working_dir, None)
            .await
    }

    async fn execute_streaming(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
        callback: Option<&dyn StreamingCallback>,
    ) -> Result<CommandOutput> {
        let (command, args) = match Self::parse_command_line(command_line) {
            Ok(parsed) => parsed,
            Err(err) => return Err(err),
        };

        let sender = match terminal_worker_sender() {
            Some(sender) => sender,
            None => {
                tracing::warn!("ACP terminal worker unavailable, falling back to local execution");
                return DefaultCommandExecutor
                    .execute_streaming(command_line, working_dir, callback)
                    .await;
            }
        };

        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let request = TerminalExecuteRequest {
            session_id: self.session_id.clone(),
            command,
            args,
            cwd: working_dir.cloned(),
            timeout: self.default_timeout,
            streaming: callback.is_some(),
            event_tx,
        };

        if sender
            .send(TerminalWorkerRequest::Execute(request))
            .is_err()
        {
            tracing::warn!(
                "Failed to dispatch ACP terminal request, falling back to local execution"
            );
            return DefaultCommandExecutor
                .execute_streaming(command_line, working_dir, callback)
                .await;
        }

        let mut final_result: Option<Result<CommandOutput>> = None;

        while let Some(event) = event_rx.recv().await {
            match event {
                TerminalWorkerEvent::TerminalAttached { terminal_id } => {
                    if let Some(cb) = callback {
                        cb.on_terminal_attached(&terminal_id)?;
                    }
                }
                TerminalWorkerEvent::OutputChunk(chunk) => {
                    if let Some(cb) = callback {
                        cb.on_output_chunk(&chunk)?;
                    }
                }
                TerminalWorkerEvent::Finished(result) => {
                    final_result = Some(result);
                    break;
                }
            }
        }

        final_result.unwrap_or_else(|| Err(anyhow!("ACP terminal worker ended without a result")))
    }
}

#[derive(Debug)]
struct TerminalExecuteRequest {
    session_id: acp::SessionId,
    command: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    timeout: Duration,
    streaming: bool,
    event_tx: UnboundedSender<TerminalWorkerEvent>,
}

enum TerminalWorkerRequest {
    Execute(TerminalExecuteRequest),
}

enum TerminalWorkerEvent {
    TerminalAttached { terminal_id: String },
    OutputChunk(String),
    Finished(Result<CommandOutput>),
}

async fn run_terminal_worker(
    connection: Arc<acp::AgentSideConnection>,
    mut rx: UnboundedReceiver<TerminalWorkerRequest>,
) {
    while let Some(message) = rx.recv().await {
        match message {
            TerminalWorkerRequest::Execute(request) => {
                execute_via_terminal(connection.clone(), request).await;
            }
        }
    }
}

async fn execute_via_terminal(
    connection: Arc<acp::AgentSideConnection>,
    request: TerminalExecuteRequest,
) {
    let event_tx = request.event_tx.clone();
    let result = run_command(connection, request, &event_tx).await;
    let _ = event_tx.send(TerminalWorkerEvent::Finished(result));
}

async fn run_command(
    connection: Arc<acp::AgentSideConnection>,
    request: TerminalExecuteRequest,
    event_tx: &UnboundedSender<TerminalWorkerEvent>,
) -> Result<CommandOutput> {
    let TerminalExecuteRequest {
        session_id,
        command,
        args,
        cwd,
        timeout,
        streaming,
        ..
    } = request;

    let create_request = acp::CreateTerminalRequest {
        session_id: session_id.clone(),
        command,
        args,
        env: Vec::new(),
        cwd,
        output_byte_limit: Some(OUTPUT_BYTE_LIMIT),
        meta: None,
    };

    let create_response = connection
        .create_terminal(create_request)
        .await
        .map_err(|e| anyhow!("Failed to create terminal: {e}"))?;

    let terminal_id = create_response.terminal_id;
    let _ = event_tx.send(TerminalWorkerEvent::TerminalAttached {
        terminal_id: terminal_id.0.as_ref().to_string(),
    });

    let result = if streaming {
        stream_terminal_output(
            connection.clone(),
            &session_id,
            &terminal_id,
            timeout,
            event_tx,
        )
        .await
    } else {
        wait_for_terminal_completion(connection.clone(), &session_id, &terminal_id, timeout).await
    };

    let release_request = acp::ReleaseTerminalRequest {
        session_id,
        terminal_id: terminal_id.clone(),
        meta: None,
    };

    match (
        result,
        connection
            .release_terminal(release_request)
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
    connection: Arc<acp::AgentSideConnection>,
    session_id: &acp::SessionId,
    terminal_id: &acp::TerminalId,
    timeout: Duration,
    event_tx: &UnboundedSender<TerminalWorkerEvent>,
) -> Result<CommandOutput> {
    let deadline = Instant::now() + timeout;
    let mut seen_len = 0usize;

    loop {
        let output_response = connection
            .terminal_output(acp::TerminalOutputRequest {
                session_id: session_id.clone(),
                terminal_id: terminal_id.clone(),
                meta: None,
            })
            .await
            .map_err(|e| anyhow!("Failed to get terminal output: {e}"))?;

        let output_current = output_response.output;

        if output_current.len() < seen_len {
            // The client truncated the buffer from the front. Reset our cursor.
            seen_len = 0;
        }

        if output_current.len() > seen_len {
            let chunk = output_current[seen_len..].to_string();
            let _ = event_tx.send(TerminalWorkerEvent::OutputChunk(chunk));
            seen_len = output_current.len();
        }

        if output_response.truncated {
            tracing::warn!("ACP terminal output truncated for session {}", session_id.0);
        }

        if let Some(status) = output_response.exit_status {
            let success = status.exit_code.map(|code| code == 0).unwrap_or(false);

            return Ok(CommandOutput {
                success,
                output: output_current,
            });
        }

        if Instant::now() >= deadline {
            let _ = connection
                .kill_terminal_command(acp::KillTerminalCommandRequest {
                    session_id: session_id.clone(),
                    terminal_id: terminal_id.clone(),
                    meta: None,
                })
                .await
                .map_err(|e| anyhow!("Failed to kill terminal after timeout: {e}"))?;

            return Err(anyhow!("Command timed out after {timeout:?}"));
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn wait_for_terminal_completion(
    connection: Arc<acp::AgentSideConnection>,
    session_id: &acp::SessionId,
    terminal_id: &acp::TerminalId,
    timeout: Duration,
) -> Result<CommandOutput> {
    let wait_future = connection.wait_for_terminal_exit(acp::WaitForTerminalExitRequest {
        session_id: session_id.clone(),
        terminal_id: terminal_id.clone(),
        meta: None,
    });

    let wait_response = tokio::time::timeout(timeout, wait_future)
        .await
        .map_err(|_| anyhow!("Command timed out after {timeout:?}"))?
        .map_err(|e| anyhow!("Failed to wait for terminal exit: {e}"))?;

    let output_response = connection
        .terminal_output(acp::TerminalOutputRequest {
            session_id: session_id.clone(),
            terminal_id: terminal_id.clone(),
            meta: None,
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_command_line_handles_quotes() {
        let (command, args) =
            ACPTerminalCommandExecutor::parse_command_line("cargo test --package \"my crate\"")
                .unwrap();
        assert_eq!(command, "cargo");
        assert_eq!(args, vec!["test", "--package", "my crate"]);
    }

    #[test]
    fn parse_command_line_errors_on_empty_input() {
        let err = ACPTerminalCommandExecutor::parse_command_line("").unwrap_err();
        assert!(err.to_string().contains("empty"));
    }
}
