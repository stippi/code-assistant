//! Blocking command execution on a backend PTY.
//!
//! Replaces the old GPUI-side terminal executor: the PTY lives in the
//! tokio world, output streams one-way to the UI as raw ANSI chunks (via
//! `StreamingCallback::on_terminal_output_chunk`), and the agent loop
//! never waits on the UI thread — the class of cross-runtime stalls the
//! old request/response worker suffered from cannot occur.
//!
//! The core of this module is [`run_pty_streaming`], which executes an
//! already-prepared [`PtySpawnSpec`]. `SandboxedCommandExecutor` routes its
//! restricted streaming executions through the same function (with a
//! seatbelt-wrapped spec), so sandboxed and unsandboxed blocking commands
//! share one code path — including cancellation via
//! `StreamingCallback::should_continue` and the terminal lifecycle
//! callbacks (`on_terminal_attached` / `on_terminal_exit`).

use crate::{
    CommandExecutor, CommandOutput, PtySpawnSpec, SandboxCommandRequest, StreamingCallback,
};
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

/// Synthetic terminal id reported via `on_terminal_attached`. Frontends key
/// terminal cards by tool_id (carried by the callback itself), so the id
/// only signals "a backend terminal now exists for this tool".
const BACKEND_TERMINAL_ID: &str = "backend-pty";

/// Execute a prepared spawn spec on a backend PTY, streaming output until
/// the process exits, the callback cancels, or the timeout strikes.
///
/// Lifecycle guarantees towards the callback:
/// - `on_terminal_attached` fires right after the spawn, **before any
///   output** — so a UI can show a live terminal card (with its stop
///   button) even for commands that stay silent, like `sleep`.
/// - `should_continue` is polled between output windows; returning `false`
///   interrupts the process (Ctrl-C to the process group) and returns what
///   was collected so far.
/// - `on_terminal_exit` fires exactly once on every path (normal exit,
///   cancellation, timeout), so a UI card never keeps spinning.
pub(crate) async fn run_pty_streaming(
    spec: PtySpawnSpec,
    working_dir: Option<&PathBuf>,
    callback: Option<&dyn StreamingCallback>,
) -> Result<CommandOutput> {
    let mut config = PtySpawnConfig::from_argv(spec.argv);
    config.env = spec.env;
    config.keep_alive = spec.keep_alive;
    config.working_dir = working_dir.cloned();

    // Blocking commands run on a PTY (for colored output), which makes
    // programs believe they are interactive: git & co. would start a pager
    // that waits forever for keyboard input. Nothing interactive can ever
    // answer a blocking command, so disable pagers unless the caller
    // explicitly set them. Interactive PTY *sessions* (execute_command's
    // session mode) intentionally do not do this.
    for (key, value) in [("PAGER", "cat"), ("GIT_PAGER", "cat")] {
        if !config.env.iter().any(|(k, _)| k == key) {
            config.env.push((key.to_string(), value.to_string()));
        }
    }

    let session = PtySession::spawn(config)?;

    // Announce the terminal before any output exists. UIs create their
    // display terminal for the tool card on this signal, so the card shows
    // a running terminal (and its stop button) even for silent commands.
    if let Some(callback) = callback {
        let _ = callback.on_terminal_attached(BACKEND_TERMINAL_ID);
    }

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
            if let Some(callback) = callback {
                let exit_code = match collected.status {
                    PtySessionStatus::Exited(code) => code,
                    // The process ignored the interrupt within the drain
                    // window; dropping `session` below kills it, so the
                    // terminal is finished either way.
                    PtySessionStatus::Running => None,
                };
                let _ = callback.on_terminal_exit(exit_code);
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
                    if let Some(callback) = callback {
                        let _ = callback.on_terminal_exit(None);
                    }
                    return Err(anyhow!("Command timed out after {DEFAULT_TIMEOUT:?}"));
                }
            }
        }
    }
}

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
        // restrictions apply, the wrapper prepares a seatbelt spec and runs
        // it through run_pty_streaming itself, never reaching this inner
        // executor.
        run_pty_streaming(PtySpawnSpec::shell(command_line), working_dir, callback).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct RecordingCallback {
        plain: Mutex<Vec<String>>,
        raw: Mutex<Vec<Vec<u8>>>,
        attached: AtomicBool,
        exits: Mutex<Vec<Option<i32>>>,
        cancel_after_polls: Option<usize>,
        polls: AtomicUsize,
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

        fn on_terminal_attached(&self, _terminal_id: &str) -> Result<()> {
            self.attached.store(true, Ordering::Relaxed);
            Ok(())
        }

        fn on_terminal_exit(&self, exit_code: Option<i32>) -> Result<()> {
            self.exits.lock().unwrap().push(exit_code);
            Ok(())
        }

        fn should_continue(&self) -> bool {
            let polls = self.polls.fetch_add(1, Ordering::Relaxed);
            match self.cancel_after_polls {
                Some(limit) => polls < limit,
                None => true,
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn runs_to_completion_with_both_stream_kinds() -> Result<()> {
        let callback = RecordingCallback::default();

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

        assert!(
            callback.attached.load(Ordering::Relaxed),
            "terminal should be announced at spawn"
        );
        assert_eq!(
            callback.exits.lock().unwrap().as_slice(),
            &[Some(0)],
            "exit should be signalled exactly once"
        );
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

    #[tokio::test(flavor = "multi_thread")]
    async fn silent_command_attaches_terminal_and_can_be_cancelled() -> Result<()> {
        // `sleep` never prints anything: the terminal must still be
        // announced (so the UI can show the card with its stop button),
        // and cancellation must work without any output having flowed.
        let callback = RecordingCallback {
            cancel_after_polls: Some(1),
            ..Default::default()
        };

        let started = std::time::Instant::now();
        let result = PtyCommandExecutor
            .execute_streaming("sleep 30", None, Some(&callback), None)
            .await?;

        assert!(!result.success, "a cancelled command is not a success");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "cancellation should end the command quickly"
        );
        assert!(
            callback.attached.load(Ordering::Relaxed),
            "terminal should be announced even without output"
        );
        assert_eq!(
            callback.exits.lock().unwrap().len(),
            1,
            "exit should be signalled exactly once on cancellation"
        );
        Ok(())
    }
}
