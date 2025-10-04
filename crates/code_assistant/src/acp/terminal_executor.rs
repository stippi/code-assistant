use anyhow::Result;
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::time::{timeout, Duration};

use agent_client_protocol::{self as acp, Client};

use crate::utils::command::{CommandExecutor, CommandOutput, StreamingCallback};

/// CommandExecutor implementation that uses ACP Terminal Protocol
/// instead of executing commands locally
pub struct ACPTerminalCommandExecutor {
    session_id: acp::SessionId,
    client: Arc<acp::AgentSideConnection>,
    default_timeout: Duration,
}

impl ACPTerminalCommandExecutor {
    pub fn new(session_id: acp::SessionId, client: Arc<acp::AgentSideConnection>) -> Self {
        Self {
            session_id,
            client,
            default_timeout: Duration::from_secs(300), // 5 minutes default timeout
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Parse command line into command and args
    fn parse_command_line(command_line: &str) -> (String, Vec<String>) {
        // Simple parsing - split on whitespace
        // In a real implementation, you might want to use a proper shell parser
        let parts: Vec<&str> = command_line.split_whitespace().collect();
        if parts.is_empty() {
            return (command_line.to_string(), vec![]);
        }

        let command = parts[0].to_string();
        let args = parts[1..].iter().map(|s| s.to_string()).collect();
        (command, args)
    }

    /// Execute command with streaming output via callback
    async fn execute_with_streaming(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
        callback: Option<&dyn StreamingCallback>,
    ) -> Result<CommandOutput> {
        let (command, args) = Self::parse_command_line(command_line);

        // Convert environment variables if needed (empty for now)
        let env = vec![];

        // Create terminal
        let create_request = acp::CreateTerminalRequest {
            session_id: self.session_id.clone(),
            command,
            args,
            env,
            cwd: working_dir.cloned(),
            output_byte_limit: Some(1_048_576), // 1MB limit
            meta: None,
        };

        let create_response = self
            .client
            .create_terminal(create_request)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create terminal: {}", e))?;

        let terminal_id = create_response.terminal_id;

        // If we have a callback, stream output in real-time
        let mut accumulated_output = String::new();
        let mut exit_status = None;

        if let Some(callback) = callback {
            // Poll for output until command completes
            loop {
                // Get current output
                let output_request = acp::TerminalOutputRequest {
                    session_id: self.session_id.clone(),
                    terminal_id: terminal_id.clone(),
                    meta: None,
                };

                let output_response = self
                    .client
                    .terminal_output(output_request)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to get terminal output: {}", e))?;

                // Send new output chunks to callback
                if output_response.output.len() > accumulated_output.len() {
                    let new_chunk = &output_response.output[accumulated_output.len()..];
                    if !new_chunk.is_empty() {
                        let _ = callback.on_output_chunk(new_chunk);
                    }
                }
                accumulated_output = output_response.output;

                // Check if command completed
                if let Some(status) = output_response.exit_status {
                    exit_status = Some(status);
                    break;
                }

                // If output was truncated, we might want to handle that
                if output_response.truncated {
                    tracing::warn!("Terminal output was truncated");
                }

                // Short delay before next poll
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        } else {
            // No streaming - just wait for completion
            let wait_request = acp::WaitForTerminalExitRequest {
                session_id: self.session_id.clone(),
                terminal_id: terminal_id.clone(),
                meta: None,
            };

            let wait_result = timeout(
                self.default_timeout,
                self.client.wait_for_terminal_exit(wait_request),
            )
            .await;

            match wait_result {
                Ok(Ok(wait_response)) => {
                    exit_status = Some(wait_response.exit_status);
                }
                Ok(Err(e)) => {
                    // Release terminal before returning error
                    let _ = self.release_terminal(&terminal_id).await;
                    return Err(anyhow::anyhow!("Failed to wait for terminal exit: {}", e));
                }
                Err(_) => {
                    // Timeout - kill the terminal
                    let _ = self.kill_terminal(&terminal_id).await;
                    let _ = self.release_terminal(&terminal_id).await;
                    return Err(anyhow::anyhow!(
                        "Command timed out after {:?}",
                        self.default_timeout
                    ));
                }
            }

            // Get final output
            let output_request = acp::TerminalOutputRequest {
                session_id: self.session_id.clone(),
                terminal_id: terminal_id.clone(),
                meta: None,
            };

            let output_response = self
                .client
                .terminal_output(output_request)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to get final terminal output: {}", e))?;

            accumulated_output = output_response.output;

            // Use the exit status from the output response if available, otherwise from wait
            if output_response.exit_status.is_some() {
                exit_status = output_response.exit_status;
            }
        }

        // Release terminal
        let _ = self.release_terminal(&terminal_id).await;

        // Determine success based on exit status
        let success = exit_status
            .as_ref()
            .and_then(|status| status.exit_code)
            .map(|code| code == 0)
            .unwrap_or(false);

        Ok(CommandOutput {
            success,
            output: accumulated_output,
        })
    }

    /// Helper to kill a terminal
    async fn kill_terminal(&self, terminal_id: &acp::TerminalId) -> Result<()> {
        let kill_request = acp::KillTerminalCommandRequest {
            session_id: self.session_id.clone(),
            terminal_id: terminal_id.clone(),
            meta: None,
        };

        self.client
            .kill_terminal_command(kill_request)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to kill terminal: {}", e))?;

        Ok(())
    }

    /// Helper to release a terminal
    async fn release_terminal(&self, terminal_id: &acp::TerminalId) -> Result<()> {
        let release_request = acp::ReleaseTerminalRequest {
            session_id: self.session_id.clone(),
            terminal_id: terminal_id.clone(),
            meta: None,
        };

        self.client
            .release_terminal(release_request)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to release terminal: {}", e))?;

        Ok(())
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
        // Since ACP client methods return non-Send futures, we need to execute them
        // in a local task context. For now, we'll fall back to the default executor
        // TODO: Implement proper ACP terminal integration with LocalSet
        tracing::warn!(
            "ACP Terminal integration not yet fully implemented - falling back to local execution"
        );

        // For now, fall back to local execution
        let default_executor = crate::utils::DefaultCommandExecutor;
        default_executor
            .execute_streaming(command_line, working_dir, callback)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_command_line() {
        let (cmd, args) = ACPTerminalCommandExecutor::parse_command_line("ls -la /tmp");
        assert_eq!(cmd, "ls");
        assert_eq!(args, vec!["-la", "/tmp"]);

        let (cmd, args) = ACPTerminalCommandExecutor::parse_command_line("echo hello");
        assert_eq!(cmd, "echo");
        assert_eq!(args, vec!["hello"]);

        let (cmd, args) = ACPTerminalCommandExecutor::parse_command_line("simple-command");
        assert_eq!(cmd, "simple-command");
        assert!(args.is_empty());
    }

    #[tokio::test]
    async fn test_executor_creation() {
        // This test would require a mock AgentSideConnection
        // For now, just test that the timeout setting works
        let session_id = acp::SessionId("test-session".to_string().into());

        // We can't easily create a mock AgentSideConnection here, so we'll skip this test
        // In a real implementation, you'd want to create a mock or test double
    }
}
