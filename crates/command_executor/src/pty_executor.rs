//! Blocking command execution on a backend PTY.
//!
//! Replaces the old GPUI-side terminal executor: the PTY lives in the
//! tokio world, output streams one-way to the UI as raw ANSI chunks (via
//! `StreamingCallback::on_terminal_output_chunk`), and the agent loop
//! never waits on the UI thread — the class of cross-runtime stalls the
//! old request/response worker suffered from cannot occur.

use crate::{CommandExecutor, CommandOutput, SandboxCommandRequest, StreamingCallback};
use anyhow::{Result, anyhow};
use pty_session::{PtySession, PtySessionStatus, PtySpawnConfig};
use std::path::PathBuf;
use std::time::Duration;

/// Default timeout for blocking commands (parity with the old GPUI
/// terminal executor).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Streaming happens per collect window; keep windows short so plain-text
/// consumers see output with bounded delay.
const STREAM_WINDOW: Duration = Duration::from_secs(2);

pub struct PtyCommandExecutor;

#[async_trait::async_trait]
impl CommandExecutor for PtyCommandExecutor {
    async fn execute(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
        sandbox_request: Option<&SandboxCommandRequest>,
    ) -> Result<CommandOutput> {
        // Non-streaming callers (format-on-save from the edit tools) don't
        // render a terminal card; skip the PTY overhead.
        crate::DefaultCommandExecutor
            .execute(command_line, working_dir, sandbox_request)
            .await
    }

    async fn execute_streaming(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
        callback: Option<&dyn StreamingCallback>,
        _sandbox_request: Option<&SandboxCommandRequest>,
    ) -> Result<CommandOutput> {
        // When this executor is wrapped by SandboxedCommandExecutor and
        // restrictions apply, the wrapper runs its seatbelt path itself and
        // never reaches this inner executor — same as the old GPUI one.
        let mut config = PtySpawnConfig::shell_command(command_line);
        config.working_dir = working_dir.cloned();

        let session = PtySession::spawn(config)?;
        let deadline = tokio::time::Instant::now() + DEFAULT_TIMEOUT;
        let mut output = String::new();

        loop {
            // A UI stop button (or other canceller) can ask us to abort
            // between windows: interrupt the process, drain a final window,
            // and return what we have so far.
            if callback.is_some_and(|c| !c.should_continue()) {
                session.interrupt();
                let collected = session.collect_output(Duration::from_millis(500)).await;
                if !collected.output.is_empty() {
                    if let Some(callback) = callback {
                        let _ = callback.on_output_chunk(&collected.output);
                    }
                    output.push_str(&collected.output);
                }
                if let PtySessionStatus::Exited(code) = collected.status
                    && let Some(callback) = callback
                {
                    let _ = callback.on_terminal_exit(code);
                }
                // Dropping `session` on return kills any process that
                // ignored the interrupt.
                return Ok(CommandOutput {
                    success: false,
                    output,
                });
            }

            let now = tokio::time::Instant::now();
            let window = (deadline - now).min(STREAM_WINDOW);

            let collected = session
                .collect_output_with(window, |bytes| {
                    if let Some(callback) = callback {
                        let _ = callback.on_terminal_output_chunk(bytes);
                    }
                })
                .await;

            if !collected.output.is_empty() {
                if let Some(callback) = callback {
                    let _ = callback.on_output_chunk(&collected.output);
                }
                output.push_str(&collected.output);
            }

            match collected.status {
                PtySessionStatus::Exited(code) => {
                    if let Some(callback) = callback {
                        let _ = callback.on_terminal_exit(code);
                    }
                    return Ok(CommandOutput {
                        success: code == Some(0),
                        output,
                    });
                }
                PtySessionStatus::Running => {
                    if tokio::time::Instant::now() >= deadline {
                        session.interrupt();
                        let _ = session.collect_output(Duration::from_millis(500)).await;
                        session.terminate();
                        return Err(anyhow!("Command timed out after {DEFAULT_TIMEOUT:?}"));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct RecordingCallback {
        plain: Mutex<Vec<String>>,
        raw: Mutex<Vec<Vec<u8>>>,
    }

    impl StreamingCallback for RecordingCallback {
        fn on_output_chunk(&self, chunk: &str) -> Result<()> {
            self.plain.lock().unwrap().push(chunk.to_string());
            Ok(())
        }

        fn on_terminal_output_chunk(&self, bytes: &[u8]) -> Result<()> {
            self.raw.lock().unwrap().push(bytes.to_vec());
            Ok(())
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn runs_to_completion_with_both_stream_kinds() -> Result<()> {
        let callback = RecordingCallback {
            plain: Mutex::new(Vec::new()),
            raw: Mutex::new(Vec::new()),
        };

        let result = PtyCommandExecutor
            .execute_streaming(
                "printf '\\033[35mmagenta\\033[0m\\n'; exit 0",
                None,
                Some(&callback),
                None,
            )
            .await?;

        assert!(result.success);
        assert!(result.output.contains("magenta"));
        assert!(
            !result.output.contains('\u{1b}'),
            "plain output must be ANSI-free: {:?}",
            result.output
        );

        let raw: Vec<u8> = callback.raw.lock().unwrap().concat();
        assert!(raw.contains(&0x1b), "raw stream should keep ANSI escapes");
        let plain = callback.plain.lock().unwrap().join("");
        assert!(plain.contains("magenta"));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nonzero_exit_reports_failure() -> Result<()> {
        let result = PtyCommandExecutor
            .execute_streaming("echo boom; exit 2", None, None, None)
            .await?;
        assert!(!result.success);
        assert!(result.output.contains("boom"));
        Ok(())
    }
}
