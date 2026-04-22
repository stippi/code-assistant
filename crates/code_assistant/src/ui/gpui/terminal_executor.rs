use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::FutureExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::{debug, warn};

use command_executor::{
    CommandExecutor, CommandOutput, DefaultCommandExecutor, SandboxCommandRequest,
    StreamingCallback,
};

use super::terminal_card_renderer::evict_cached_terminal_view_for_tool;
use super::terminal_pool;
use crate::session::diag;
use gpui::{AppContext as _, Entity};
use terminal::{StyledLine, Terminal};

// ---------------------------------------------------------------------------
// Styled output cache — preserves ANSI colors for static terminal cards
// ---------------------------------------------------------------------------

/// Cache of styled terminal output captured just before terminal cleanup.
/// Keyed by tool_id, this allows the terminal card renderer to display
/// colored output even after the live PTY terminal has been destroyed.
static STYLED_OUTPUT_CACHE: OnceLock<Mutex<HashMap<String, Vec<StyledLine>>>> = OnceLock::new();

fn styled_output_cache() -> &'static Mutex<HashMap<String, Vec<StyledLine>>> {
    STYLED_OUTPUT_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Maximum entries in the styled output cache. If this is exceeded, the oldest
/// entries are evicted. This prevents unbounded memory growth if
/// `take_cached_styled_output` is never called for some tool_ids.
const MAX_STYLED_CACHE_ENTRIES: usize = 32;

/// Store styled output for a tool_id. Called just before terminal cleanup.
fn cache_styled_output(tool_id: &str, styled_lines: Vec<StyledLine>) {
    if let Ok(mut cache) = styled_output_cache().lock() {
        // Evict old entries if the cache is too large
        while cache.len() >= MAX_STYLED_CACHE_ENTRIES {
            if let Some(key) = cache.keys().next().cloned() {
                cache.remove(&key);
            } else {
                break;
            }
        }
        cache.insert(tool_id.to_string(), styled_lines);
    }
}

/// Retrieve and remove cached styled output for a tool_id.
/// Called by the terminal card renderer when transitioning to static display.
pub fn take_cached_styled_output(tool_id: &str) -> Option<Vec<StyledLine>> {
    styled_output_cache()
        .lock()
        .ok()
        .and_then(|mut cache| cache.remove(tool_id))
}

/// Capture a `(pool_active, pool_total, open_fds)` snapshot for diag logs.
/// Lock is taken non-blocking (try_lock) so diag never contends with hot
/// terminal ops; returns `"?"` values if the pool is held elsewhere.
fn resource_snapshot() -> String {
    match terminal_pool::TerminalPool::global().try_lock() {
        Ok(pool) => {
            let (active, total) = pool.stats();
            diag::resource_snapshot(active, total)
        }
        Err(_) => format!(
            "pool_active=? pool_total=? open_fds={}",
            diag::open_fd_count()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".into())
        ),
    }
}

fn cleanup_terminal_resources(terminal_id: &str) {
    if let Ok(mut pool) = terminal_pool::TerminalPool::global().lock() {
        for tool_id in pool.remove(terminal_id) {
            evict_cached_terminal_view_for_tool(&tool_id);
        }
    }
}

struct TerminalCleanup {
    terminal_id: String,
}

impl TerminalCleanup {
    fn new(terminal_id: String) -> Self {
        Self { terminal_id }
    }
}

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        cleanup_terminal_resources(&self.terminal_id);
    }
}

fn interrupt_terminal(terminal: &Entity<Terminal>, cx: &mut gpui::AsyncApp) {
    let _ = cx.update_entity(terminal, |terminal: &mut Terminal, _cx| {
        terminal.write_to_pty(&b"\x03"[..]);
    });
}

/// Default timeout for commands (5 minutes).
const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Global sender to dispatch requests to the GPUI foreground worker.
static TERMINAL_WORKER: OnceLock<UnboundedSender<TerminalWorkerRequest>> = OnceLock::new();

/// Millisecond-since-epoch of the last time `run_terminal_worker` made progress
/// (either entered `rx.recv().await` or returned from it). The tokio-side
/// liveness timeout reads this so its error message can distinguish
/// "worker loop is still alive but the channel is wedged" from
/// "foreground task is frozen / starved".
static WORKER_HEARTBEAT_MS: AtomicU64 = AtomicU64::new(0);

fn heartbeat_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn heartbeat_bump() {
    WORKER_HEARTBEAT_MS.store(heartbeat_now_ms(), Ordering::Relaxed);
}

/// Format "age since last heartbeat". Returns `"never"` if the worker never
/// ran, a millisecond age otherwise.
fn heartbeat_age_desc() -> String {
    let last = WORKER_HEARTBEAT_MS.load(Ordering::Relaxed);
    if last == 0 {
        return "never".to_string();
    }
    let now = heartbeat_now_ms();
    format!("{}ms_ago", now.saturating_sub(last))
}

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
pub struct GpuiTerminalCommandExecutor {
    session_id: String,
}

impl GpuiTerminalCommandExecutor {
    pub fn new(session_id: String) -> Self {
        Self { session_id }
    }
}

#[async_trait]
impl CommandExecutor for GpuiTerminalCommandExecutor {
    async fn execute(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
        sandbox_request: Option<&SandboxCommandRequest>,
    ) -> Result<CommandOutput> {
        // Non-streaming callers (format-on-save from edit / replace_in_file /
        // write_file) don't need a PTY and don't render a terminal card. Route
        // them through the plain process executor so a stuck PTY worker can't
        // park the agent loop inside an internal command.
        //
        // When the outer SandboxedCommandExecutor requires restrictions it
        // doesn't reach this inner executor in the first place — it runs the
        // seatbelt invocation directly — so using DefaultCommandExecutor here
        // doesn't bypass sandboxing.
        DefaultCommandExecutor
            .execute(command_line, working_dir, sandbox_request)
            .await
    }

    async fn execute_streaming(
        &self,
        command_line: &str,
        working_dir: Option<&PathBuf>,
        callback: Option<&dyn StreamingCallback>,
        sandbox_request: Option<&SandboxCommandRequest>,
    ) -> Result<CommandOutput> {
        let sid = self.session_id.as_str();
        diag::log(
            sid,
            format_args!(
                "GpuiExec::execute_streaming: entered {} cmd={:?}",
                resource_snapshot(),
                command_line
            ),
        );

        let sender = match terminal_worker_sender() {
            Some(sender) => sender,
            None => {
                warn!("GPUI terminal worker unavailable, falling back to local execution");
                diag::log(
                    sid,
                    "GpuiExec::execute_streaming: worker unavailable, falling back to DefaultCommandExecutor",
                );
                return DefaultCommandExecutor
                    .execute_streaming(command_line, working_dir, callback, sandbox_request)
                    .await;
            }
        };

        // Create a channel for the worker to send events back.
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let tool_id = callback.and_then(|cb| cb.tool_id().map(|s| s.to_string()));

        let request = TerminalExecuteRequest {
            command_line: command_line.to_string(),
            cwd: working_dir.cloned(),
            timeout: DEFAULT_TIMEOUT,
            event_tx,
            session_id: self.session_id.clone(),
            tool_id: tool_id.clone(),
        };

        if sender
            .send(TerminalWorkerRequest::Execute(request))
            .is_err()
        {
            warn!("Failed to dispatch GPUI terminal request, falling back to local execution");
            diag::log(
                sid,
                "GpuiExec::execute_streaming: worker send() failed, falling back to DefaultCommandExecutor",
            );
            return DefaultCommandExecutor
                .execute_streaming(command_line, working_dir, callback, sandbox_request)
                .await;
        }

        diag::log(
            sid,
            format_args!(
                "GpuiExec::execute_streaming: dispatched request to worker tool_id={:?}",
                tool_id
            ),
        );

        // Process events from the worker.
        //
        // Liveness timeout (safety net above the in-worker 5-min timeout):
        // if the GPUI foreground task never runs — e.g. starvation — we'd
        // otherwise wait forever. Cap this loop at a value slightly greater
        // than DEFAULT_TIMEOUT so the normal path always wins when healthy.
        let overall_deadline = DEFAULT_TIMEOUT + std::time::Duration::from_secs(30);
        let started = std::time::Instant::now();
        let mut final_result: Option<Result<CommandOutput>> = None;

        loop {
            let remaining = overall_deadline.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                diag::log(
                    sid,
                    format_args!(
                        "GpuiExec::execute_streaming: tokio-side liveness timeout after {:?} (worker never reported Finished) tool_id={:?} worker_heartbeat={} {}",
                        overall_deadline,
                        tool_id,
                        heartbeat_age_desc(),
                        resource_snapshot()
                    ),
                );
                return Err(anyhow!(
                    "GPUI terminal worker did not report completion within {overall_deadline:?}"
                ));
            }

            let recv = tokio::time::timeout(remaining, event_rx.recv()).await;
            match recv {
                Err(_elapsed) => {
                    // Loop; will hit the deadline check above on next iteration.
                    continue;
                }
                Ok(None) => {
                    diag::log(
                        sid,
                        format_args!(
                            "GpuiExec::execute_streaming: event channel closed tool_id={:?}",
                            tool_id
                        ),
                    );
                    break;
                }
                Ok(Some(event)) => match event {
                    TerminalWorkerEvent::TerminalAttached { terminal_id } => {
                        diag::log(
                            sid,
                            format_args!(
                                "GpuiExec::execute_streaming: got TerminalAttached terminal_id={terminal_id} tool_id={:?}",
                                tool_id
                            ),
                        );
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
                        diag::log(
                            sid,
                            format_args!(
                                "GpuiExec::execute_streaming: got Finished is_ok={} tool_id={:?} {}",
                                result.is_ok(),
                                tool_id,
                                resource_snapshot()
                            ),
                        );
                        final_result = Some(result);
                        break;
                    }
                },
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
    session_id: String,
    tool_id: Option<String>,
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
    // Stamp heartbeat before the first recv so the tokio side can tell this
    // task at least started.
    heartbeat_bump();
    loop {
        let msg = rx.recv().await;
        heartbeat_bump();
        match msg {
            Some(TerminalWorkerRequest::Execute(request)) => {
                diag::log(
                    &request.session_id,
                    format_args!(
                        "run_terminal_worker: received Execute, spawning execute_in_terminal tool_id={:?} cmd={:?} {}",
                        request.tool_id, request.command_line, resource_snapshot()
                    ),
                );
                // Spawn each execution as an independent task so multiple
                // commands can run concurrently.
                cx.spawn(async move |cx| {
                    execute_in_terminal(request, cx).await;
                })
                .detach();
            }
            None => {
                // All senders dropped — shouldn't happen because TERMINAL_WORKER
                // holds one, but exit cleanly if it does.
                break;
            }
        }
    }
}

async fn execute_in_terminal(request: TerminalExecuteRequest, cx: &mut gpui::AsyncApp) {
    let event_tx = request.event_tx.clone();
    let sid = request.session_id.clone();
    let tool_id = request.tool_id.clone();
    diag::log(
        &sid,
        format_args!(
            "execute_in_terminal: start tool_id={:?} {}",
            tool_id,
            resource_snapshot()
        ),
    );
    let result = run_command(request, cx).await;
    diag::log(
        &sid,
        format_args!(
            "execute_in_terminal: run_command returned is_ok={} tool_id={:?} {}",
            result.is_ok(),
            tool_id,
            resource_snapshot()
        ),
    );
    let send_err = event_tx
        .send(TerminalWorkerEvent::Finished(result))
        .is_err();
    if send_err {
        diag::log(
            &sid,
            format_args!(
                "execute_in_terminal: event_tx.send(Finished) failed (receiver dropped) tool_id={:?}",
                tool_id
            ),
        );
    }
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
        session_id,
        tool_id,
    } = request;
    let sid = session_id.clone();

    diag::log(
        &sid,
        format_args!(
            "run_command: entering, about to spawn PTY tool_id={:?} cmd={:?} {}",
            tool_id,
            command_line,
            resource_snapshot()
        ),
    );

    // Channels for forwarding terminal events out of the GPUI callback
    // context. These must be created BEFORE the terminal entity so the
    // subscription can be installed atomically with the spawn below.
    let (exit_tx, mut exit_rx) = mpsc::unbounded_channel::<Option<i32>>();
    let (wakeup_tx, mut wakeup_rx) = mpsc::unbounded_channel::<()>();

    // Create the PTY terminal AND install the event subscription inside the
    // same `cx.update` so no event can be dispatched between the two.
    // Previously the subscription was installed in a second `cx.update`,
    // which opened a window where a fast-exiting child could emit
    // `ChildExit` before we were listening — leaving the worker stuck
    // polling for an event that had already been delivered and dropped.
    let spawn_result = cx.update(|cx| -> Result<_, anyhow::Error> {
        let (id, entity) = terminal_pool::spawn_terminal_in_pool(&command_line, cwd.as_deref(), cx)
            .map_err(|e| anyhow!("Failed to create PTY terminal: {e}"))?;

        let exit_tx = exit_tx.clone();
        let wakeup_tx = wakeup_tx.clone();

        // Detach the subscription rather than holding it for the lifetime of
        // `run_command`. The subscription naturally dies when the Entity<Terminal>
        // is dropped (which happens via `TerminalCleanup` right after we return),
        // and that path already tears everything down cleanly.
        //
        // Explicitly dropping the Subscription introduced an extra ordering
        // step: subscription-teardown first, *then* entity-drop inside the
        // pool mutex. On the hang we observed in the diag log, a subsequent
        // run_command never saw its Execute request picked up — consistent
        // with a cross-thread interaction between Subscription::drop and the
        // entity-drop under the pool mutex. Detaching removes that edge.
        cx.subscribe(&entity, move |_terminal, event, _cx| match event {
            terminal::Event::ChildExit(code) => {
                let _ = exit_tx.send(*code);
            }
            terminal::Event::Wakeup => {
                let _ = wakeup_tx.send(());
            }
            _ => {}
        })
        .detach();

        Ok((id, entity))
    });

    let (terminal_id, terminal) = match spawn_result {
        Ok(Ok(tuple)) => tuple,
        Ok(Err(e)) => {
            diag::log(
                &sid,
                format_args!(
                    "run_command: spawn_terminal_in_pool failed tool_id={:?} err={e}",
                    tool_id
                ),
            );
            return Err(e);
        }
        Err(e) => {
            diag::log(
                &sid,
                format_args!(
                    "run_command: cx.update for spawn failed tool_id={:?} err={e}",
                    tool_id
                ),
            );
            return Err(e);
        }
    };

    debug!("GPUI terminal {terminal_id} created for command: {command_line}");
    diag::log(
        &sid,
        format_args!(
            "run_command: PTY created terminal_id={terminal_id} tool_id={:?} {}",
            tool_id,
            resource_snapshot()
        ),
    );
    let _cleanup = TerminalCleanup::new(terminal_id.clone());

    // Register the tool → terminal mapping immediately so the UI can find
    // the live terminal as soon as it renders the tool card.  Previously this
    // mapping was established via a round-trip through the event queue
    // (executor → callback → ProxyUI → GPUI event → pool.register), which
    // caused a race: the tool card could render in "Running" state before
    // the mapping arrived, showing a skeleton forever.
    if let Some(ref tool_id) = tool_id {
        if let Ok(mut pool) = terminal_pool::TerminalPool::global().lock() {
            pool.register_tool_mapping(session_id, tool_id.clone(), terminal_id.clone());
        }
    }

    // Notify the caller that the terminal is attached.
    let _ = event_tx.send(TerminalWorkerEvent::TerminalAttached {
        terminal_id: terminal_id.clone(),
    });

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
        // Enforce timeout on every iteration, not just when `poll_timer`
        // wins the `select_biased!` race below. Under a chatty command
        // the wakeup branch can fire continuously and starve the poll
        // timer, which would make the timeout ineffective.
        if started_at.elapsed() >= timeout {
            warn!(
                "GPUI terminal {terminal_id} timed out after {timeout:?} for command: {command_line}"
            );
            diag::log(
                &sid,
                format_args!(
                    "run_command: TIMEOUT terminal_id={terminal_id} tool_id={:?} elapsed={:?} {}",
                    tool_id,
                    started_at.elapsed(),
                    resource_snapshot()
                ),
            );
            interrupt_terminal(&terminal, cx);
            return Err(anyhow!("Command timed out after {timeout:?}"));
        }

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
                    diag::log(
                        &sid,
                        format_args!(
                            "run_command: exit_rx closed unexpectedly terminal_id={terminal_id} tool_id={:?}",
                            tool_id
                        ),
                    );
                    return Err(anyhow!("Terminal exit channel closed unexpectedly"));
                };

                diag::log(
                    &sid,
                    format_args!(
                        "run_command: ChildExit code={:?} terminal_id={terminal_id} tool_id={:?} elapsed={:?}",
                        exit_code, tool_id, started_at.elapsed()
                    ),
                );


                // Child exited. Read final output and styled content.
                let (output, styled) = cx.update(|cx| {
                    let t = terminal.read(cx);
                    (t.get_content_text(), t.get_styled_content())
                })?;

                // Cache styled output for the terminal card renderer
                if let Some(tid) = &tool_id {
                    cache_styled_output(tid, styled);
                }

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
                    diag::log(
                        &sid,
                        format_args!(
                            "run_command: wakeup_rx closed unexpectedly terminal_id={terminal_id} tool_id={:?}",
                            tool_id
                        ),
                    );
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
                    diag::log(
                        &sid,
                        format_args!(
                            "run_command: exit via wakeup fallback terminal_id={terminal_id} tool_id={:?} elapsed={:?}",
                            tool_id, started_at.elapsed()
                        ),
                    );

                    // Cache styled output before terminal cleanup
                    if let Some(tid) = &tool_id {
                        if let Ok(styled) = cx.update(|cx| terminal.read(cx).get_styled_content()) {
                            cache_styled_output(tid, styled);
                        }
                    }

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
            // (The timeout is enforced at the top of the loop.)

            _ = poll_timer.fuse() => {
                let (exited, exit_status, output) = cx.update(|cx| {
                    let t = terminal.read(cx);
                    (t.has_exited(), t.exit_status(), t.get_content_text())
                })?;

                if exited {
                    debug!("Terminal exit detected via periodic poll fallback");
                    diag::log(
                        &sid,
                        format_args!(
                            "run_command: exit via poll fallback terminal_id={terminal_id} tool_id={:?} elapsed={:?}",
                            tool_id, started_at.elapsed()
                        ),
                    );

                    // Cache styled output before terminal cleanup
                    if let Some(tid) = &tool_id {
                        if let Ok(styled) = cx.update(|cx| terminal.read(cx).get_styled_content()) {
                            cache_styled_output(tid, styled);
                        }
                    }

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
