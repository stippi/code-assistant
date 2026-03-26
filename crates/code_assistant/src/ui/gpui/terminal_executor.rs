use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::FutureExt;
use std::path::PathBuf;
use std::sync::OnceLock;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::{debug, warn};

use command_executor::{
    CommandExecutor, CommandOutput, DefaultCommandExecutor, SandboxCommandRequest,
    StreamingCallback,
};

use super::terminal_pool;

/// Default timeout for commands (5 minutes).
const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Global sender to dispatch requests to the GPUI foreground worker.
static TERMINAL_WORKER: OnceLock<UnboundedSender<TerminalWorkerRequest>> = OnceLock::new();

// ---------------------------------------------------------------------------
// Worker registration (called from the GPUI foreground thread)
// ---------------------------------------------------------------------------

/// Register the GPUI terminal worker. Must be called from a GPUI `cx.spawn()`
/// context so the worker task runs on the GPUI foreground thread and can
/// create `Entity<Terminal>` instances.
pub fn register_gpui_terminal_worker(cx: &mut gpui::AsyncApp) {
    if TERMINAL_WORKER.get().is_some() {
        warn!("GPUI terminal worker already registered");
        return;
    }

    let (tx, rx) = mpsc::unbounded_channel();
    match TERMINAL_WORKER.set(tx) {
        Ok(()) => {
            // Spawn the worker as a background task that we detach.
            // It runs on the GPUI foreground thread (we're inside cx.spawn).
            cx.spawn(async move |cx| {
                run_terminal_worker(rx, cx).await;
            })
            .detach();
            debug!("GPUI terminal worker registered");
        }
        Err(_) => {
            warn!("GPUI terminal worker registration raced");
        }
    }
}

fn terminal_worker_sender() -> Option<UnboundedSender<TerminalWorkerRequest>> {
    TERMINAL_WORKER.get().cloned()
}

// ---------------------------------------------------------------------------
// GpuiTerminalCommandExecutor
// ---------------------------------------------------------------------------

/// A `CommandExecutor` that creates real PTY terminals on the GPUI foreground
/// thread. Commands run inside an alacritty terminal emulator, so the UI can
/// display live ANSI-colored output.
///
/// If the GPUI worker is not available (e.g. running without GUI), it falls
/// back to `DefaultCommandExecutor`.
pub struct GpuiTerminalCommandExecutor;

#[async_trait]
impl CommandExecutor for GpuiTerminalCommandExecutor {
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
        sandbox_request: Option<&SandboxCommandRequest>,
    ) -> Result<CommandOutput> {
        let sender = match terminal_worker_sender() {
            Some(sender) => sender,
            None => {
                warn!("GPUI terminal worker unavailable, falling back to local execution");
                return DefaultCommandExecutor
                    .execute_streaming(command_line, working_dir, callback, sandbox_request)
                    .await;
            }
        };

        // Create a channel for the worker to send events back.
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let request = TerminalExecuteRequest {
            command_line: command_line.to_string(),
            cwd: working_dir.cloned(),
            timeout: DEFAULT_TIMEOUT,
            event_tx,
        };

        if sender
            .send(TerminalWorkerRequest::Execute(request))
            .is_err()
        {
            warn!("Failed to dispatch GPUI terminal request, falling back to local execution");
            return DefaultCommandExecutor
                .execute_streaming(command_line, working_dir, callback, sandbox_request)
                .await;
        }

        // Process events from the worker.
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

        final_result.unwrap_or_else(|| Err(anyhow!("GPUI terminal worker ended without a result")))
    }
}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

struct TerminalExecuteRequest {
    command_line: String,
    cwd: Option<PathBuf>,
    timeout: std::time::Duration,
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

// ---------------------------------------------------------------------------
// Worker loop (runs on GPUI foreground thread)
// ---------------------------------------------------------------------------

async fn run_terminal_worker(
    mut rx: UnboundedReceiver<TerminalWorkerRequest>,
    cx: &mut gpui::AsyncApp,
) {
    while let Some(msg) = rx.recv().await {
        match msg {
            TerminalWorkerRequest::Execute(request) => {
                // Spawn each execution as an independent task so multiple
                // commands can run concurrently.
                cx.spawn(async move |cx| {
                    execute_in_terminal(request, cx).await;
                })
                .detach();
            }
        }
    }
}

async fn execute_in_terminal(request: TerminalExecuteRequest, cx: &mut gpui::AsyncApp) {
    let event_tx = request.event_tx.clone();
    let result = run_command(request, cx).await;
    let _ = event_tx.send(TerminalWorkerEvent::Finished(result));
}

async fn run_command(
    request: TerminalExecuteRequest,
    cx: &mut gpui::AsyncApp,
) -> Result<CommandOutput> {
    let TerminalExecuteRequest {
        command_line,
        cwd,
        timeout,
        event_tx,
        ..
    } = request;

    // Create the PTY terminal on the GPUI thread.
    let (terminal_id, terminal) = cx
        .update(|cx| terminal_pool::spawn_terminal_in_pool(&command_line, cwd.as_deref(), cx))?
        .map_err(|e| anyhow!("Failed to create PTY terminal: {e}"))?;

    debug!("GPUI terminal {terminal_id} created for command: {command_line}");

    // Notify the caller that the terminal is attached.
    let _ = event_tx.send(TerminalWorkerEvent::TerminalAttached {
        terminal_id: terminal_id.clone(),
    });

    // Subscribe to terminal events on the GPUI thread.
    // We use a tokio channel to bridge GPUI events to our async context.
    let (exit_tx, mut exit_rx) = mpsc::unbounded_channel::<Option<i32>>();
    let (wakeup_tx, mut wakeup_rx) = mpsc::unbounded_channel::<()>();

    cx.update(|cx| {
        // Subscribe to terminal events
        let exit_tx_clone = exit_tx.clone();
        let wakeup_tx_clone = wakeup_tx.clone();
        // We keep the subscription alive by leaking it — the terminal will be
        // dropped when it exits and the pool cleans up, which will also drop
        // the subscription implicitly. Alternatively we could store it, but
        // since the terminal lifetime is bounded this is acceptable.
        let _sub = cx.subscribe(&terminal, move |_terminal, event, _cx| match event {
            terminal::Event::ChildExit(code) => {
                let _ = exit_tx_clone.send(*code);
            }
            terminal::Event::Wakeup => {
                let _ = wakeup_tx_clone.send(());
            }
            _ => {}
        });
        // Intentionally leak the subscription — it will be cleaned up when the
        // terminal entity is dropped.
        std::mem::forget(_sub);
    })?;

    // Track the last output length we've sent (in chars, not bytes, since
    // get_content_text() can reflow between reads and byte offsets become
    // invalid across multi-byte UTF-8 characters).
    let mut seen_chars = 0usize;
    let started_at = std::time::Instant::now();

    // IMPORTANT: This function runs on the GPUI foreground thread (inside
    // cx.spawn()), NOT on a tokio runtime. tokio::select!, tokio::time::sleep,
    // and tokio::time::sleep_until require the tokio timer driver and will
    // either panic or never resolve on GPUI's async executor. We use
    // futures::select! with GPUI-native timers instead.

    loop {
        // Create a GPUI-native timer for the periodic poll. This uses GPUI's
        // background executor which has its own timer implementation that works
        // regardless of the async runtime.
        let poll_timer = cx
            .background_executor()
            .timer(std::time::Duration::from_millis(500));

        // Use futures::select! which is runtime-agnostic (no tokio dependency).
        futures::select_biased! {
            exit_code = exit_rx.recv().fuse() => {
                let Some(exit_code) = exit_code else {
                    return Err(anyhow!("Terminal exit channel closed unexpectedly"));
                };

                // Child exited. Read final output.
                let output = cx.update(|cx| {
                    terminal.read(cx).get_content_text()
                })?;

                // Send any remaining output chunk.
                let total_chars = output.chars().count();
                if total_chars > seen_chars {
                    let chunk: String = output.chars().skip(seen_chars).collect();
                    let _ = event_tx.send(TerminalWorkerEvent::OutputChunk(chunk));
                }

                let success = exit_code.map(|c| c == 0).unwrap_or(false);
                return Ok(CommandOutput { success, output });
            }
            wakeup = wakeup_rx.recv().fuse() => {
                if wakeup.is_none() {
                    return Err(anyhow!("Terminal wakeup channel closed unexpectedly"));
                }

                // Terminal content changed. Stream the delta.
                let (output, exited, exit_status) = cx.update(|cx| {
                    let t = terminal.read(cx);
                    (t.get_content_text(), t.has_exited(), t.exit_status())
                })?;

                let total_chars = output.chars().count();
                if total_chars > seen_chars {
                    let chunk: String = output.chars().skip(seen_chars).collect();
                    let _ = event_tx.send(TerminalWorkerEvent::OutputChunk(chunk));
                    seen_chars = total_chars;
                }

                // Polling fallback: if the terminal has exited but we never
                // received ChildExit via subscription (can happen due to event
                // ordering in GPUI), detect it here and return.
                if exited {
                    debug!("Terminal exit detected via wakeup polling fallback");
                    let success = exit_status
                        .flatten()
                        .map(|c| c == 0)
                        .unwrap_or(false);
                    return Ok(CommandOutput { success, output });
                }
            }
            // Periodic poll: check for exit even if no events arrive.
            // This catches the case where ChildExit is emitted but the
            // subscription callback or wakeup events are not delivered.
            _ = poll_timer.fuse() => {
                // Check timeout first.
                if started_at.elapsed() >= timeout {
                    return Err(anyhow!("Command timed out after {timeout:?}"));
                }

                let (exited, exit_status, output) = cx.update(|cx| {
                    let t = terminal.read(cx);
                    (t.has_exited(), t.exit_status(), t.get_content_text())
                })?;

                if exited {
                    debug!("Terminal exit detected via periodic poll fallback");
                    let total_chars = output.chars().count();
                    if total_chars > seen_chars {
                        let chunk: String = output.chars().skip(seen_chars).collect();
                        let _ = event_tx.send(TerminalWorkerEvent::OutputChunk(chunk));
                    }
                    let success = exit_status
                        .flatten()
                        .map(|c| c == 0)
                        .unwrap_or(false);
                    return Ok(CommandOutput { success, output });
                }
            }
        }
    }
}
