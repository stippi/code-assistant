//! A single interactive process, optionally attached to a PTY.
//!
//! Unlike `command_executor`, which blocks until the child exits and then
//! returns all output, a [`PtySession`] stays alive across tool calls: the
//! caller polls output windows via [`PtySession::collect_output`] and can
//! keep feeding stdin via [`PtySession::write`]. This is what enables
//! interactive programs (ssh logins, REPLs, sudo prompts) and background
//! processes the agent checks on later.
//!
//! Process model:
//! - `tty: true` — spawned through `portable-pty` with a controlling
//!   terminal (the PTY child is its own session leader), stdin writable.
//! - `tty: false` — plain pipes via `tokio::process`, stdin closed. Writing
//!   returns an error advising to re-run with `tty: true`.

use crate::buffer::{BufferedBytes, HeadTailBuffer};
use anyhow::{Context as _, Result, anyhow};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, watch};
use tracing::warn;

/// Default cap for output retained between two `collect_output` calls.
pub const DEFAULT_MAX_BUFFER_BYTES: usize = 1024 * 1024;

/// After the child exits, wait this long for trailing output to drain
/// before returning from `collect_output`.
const EXIT_OUTPUT_GRACE: Duration = Duration::from_millis(100);

/// ASCII ETX — what pressing Ctrl-C sends on a terminal.
pub const CTRL_C: &[u8] = b"\x03";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtySessionStatus {
    Running,
    /// The process exited. `None` when no exit code is known (e.g. killed
    /// by a signal on the pipe path).
    Exited(Option<i32>),
}

/// One output window drained from the session.
pub struct CollectedOutput {
    /// Plain-text output: ANSI escape sequences stripped, CR/CRLF
    /// normalized to LF — what a language model should read. Consumers
    /// that want the raw bytes (terminal renderers) take them via the
    /// `on_chunk` callback of [`PtySession::collect_output_with`].
    pub output: String,
    /// Bytes dropped from the middle of the window when it exceeded the cap.
    pub omitted_bytes: usize,
    pub status: PtySessionStatus,
}

/// Receives every raw output chunk of a session as it arrives, for the
/// whole lifetime of the session — including between tool calls, so a
/// background process keeps streaming to its terminal card while the agent
/// does other work. `emit` is called from the reader thread and must not
/// block (a UI sink should hand the bytes to a channel/broadcast).
pub trait TerminalOutputSink: Send + Sync {
    fn emit(&self, bytes: &[u8]);

    /// Called once when the process exits, with its exit code (`None` when
    /// no code is known, e.g. killed by a signal). Lets a UI sink mark its
    /// terminal card finished even for a background session the agent never
    /// polls again. Default no-op for sinks that only care about output.
    fn on_exit(&self, _exit_code: Option<i32>) {}
}

/// Strip ANSI escape sequences and normalize line endings for LLM/text
/// consumption of terminal output.
pub fn sanitize_terminal_output(bytes: &[u8]) -> String {
    // Normalize CR before stripping: PTYs emit CRLF, and progress-bar
    // style updates use lone CR — which strip() would silently drop,
    // gluing successive updates onto one line.
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut iter = bytes.iter().peekable();
    while let Some(&byte) = iter.next() {
        if byte == b'\r' {
            if iter.peek() != Some(&&b'\n') {
                normalized.push(b'\n');
            }
        } else {
            normalized.push(byte);
        }
    }
    let stripped = strip_ansi_escapes::strip(&normalized);
    String::from_utf8_lossy(&stripped).into_owned()
}

pub struct PtySpawnConfig {
    /// Program and arguments. Use [`PtySpawnConfig::shell_command`] for the
    /// common "run this command line through the user's shell" case;
    /// explicit argv exists so callers can wrap the invocation (e.g. in a
    /// sandbox executable).
    pub argv: Vec<String>,
    pub working_dir: Option<PathBuf>,
    /// Extra environment variables (the parent environment is inherited).
    pub env: Vec<(String, String)>,
    pub tty: bool,
    pub max_buffer_bytes: usize,
    /// Opaque guard kept alive as long as the session runs — e.g. the
    /// temp file holding a sandbox profile the spawned argv references.
    pub keep_alive: Option<Box<dyn std::any::Any + Send>>,
    /// When set, every raw output chunk is forwarded here for the session's
    /// whole lifetime (live terminal streaming, independent of polling).
    pub output_sink: Option<Arc<dyn TerminalOutputSink>>,
}

impl PtySpawnConfig {
    pub fn from_argv(argv: Vec<String>) -> Self {
        Self {
            argv,
            working_dir: None,
            env: Vec::new(),
            tty: true,
            max_buffer_bytes: DEFAULT_MAX_BUFFER_BYTES,
            keep_alive: None,
            output_sink: None,
        }
    }

    /// Run a command line through the user's shell, like the classic
    /// `execute_command` path does.
    pub fn shell_command(command_line: &str) -> Self {
        #[cfg(target_family = "unix")]
        let argv = vec![
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string()),
            "-c".to_string(),
            command_line.to_string(),
        ];
        #[cfg(target_family = "windows")]
        let argv = vec![
            "cmd".to_string(),
            "/C".to_string(),
            command_line.to_string(),
        ];
        Self::from_argv(argv)
    }
}

/// Handle used to stop the process (group).
struct Terminator {
    /// On unix both spawn paths make the child its own process group
    /// leader, so signalling `-pid` reaps descendants too.
    #[cfg(target_family = "unix")]
    pgid: Option<i32>,
    /// Fallback killer (non-unix, or when no pid was reported).
    killer: Option<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
}

impl Terminator {
    fn signal(&mut self, #[cfg_attr(not(target_family = "unix"), allow(unused))] signal: i32) {
        #[cfg(target_family = "unix")]
        if let Some(pgid) = self.pgid {
            unsafe {
                libc::kill(-pgid, signal);
            }
            return;
        }
        if let Some(killer) = self.killer.as_mut() {
            if let Err(e) = killer.kill() {
                warn!("Failed to kill pty session child: {e}");
            }
        } else {
            warn!("No terminator available for pty session child");
        }
    }
}

pub struct PtySession {
    buffer: Arc<Mutex<HeadTailBuffer>>,
    /// Signalled by the reader tasks whenever new output was appended.
    output_notify: Arc<Notify>,
    max_buffer_bytes: usize,
    exit_rx: watch::Receiver<Option<Option<i32>>>,
    /// Present only for `tty: true` sessions.
    writer_tx: Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>,
    terminator: Mutex<Terminator>,
    /// Keeps the PTY master (and thus the PTY itself) alive for the
    /// session's lifetime; dropping it would hang up the child. The Mutex
    /// exists for `Sync` (and a future `resize()`), not for contention.
    _master: Option<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    /// See [`PtySpawnConfig::keep_alive`]. The Mutex exists for `Sync`.
    _keep_alive: Mutex<Option<Box<dyn std::any::Any + Send>>>,
    started_at: Instant,
    tty: bool,
}

impl PtySession {
    /// Spawn the process described by `config`. Must be called from within
    /// a tokio runtime (the pipe path spawns reader tasks on it).
    pub fn spawn(config: PtySpawnConfig) -> Result<Self> {
        if config.argv.is_empty() {
            return Err(anyhow!("PtySpawnConfig.argv must not be empty"));
        }
        if let Some(dir) = &config.working_dir
            && !dir.is_dir()
        {
            return Err(anyhow!(
                "Working directory does not exist or is not a directory: {}",
                dir.display()
            ));
        }

        let buffer = Arc::new(Mutex::new(HeadTailBuffer::new(config.max_buffer_bytes)));
        let output_notify = Arc::new(Notify::new());
        let (exit_tx, exit_rx) = watch::channel(None);

        if config.tty {
            Self::spawn_tty(config, buffer, output_notify, exit_tx, exit_rx)
        } else {
            Self::spawn_piped(config, buffer, output_notify, exit_tx, exit_rx)
        }
    }

    fn spawn_tty(
        config: PtySpawnConfig,
        buffer: Arc<Mutex<HeadTailBuffer>>,
        output_notify: Arc<Notify>,
        exit_tx: watch::Sender<Option<Option<i32>>>,
        exit_rx: watch::Receiver<Option<Option<i32>>>,
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| anyhow!("Failed to open PTY: {e}"))?;

        let mut cmd = CommandBuilder::new(&config.argv[0]);
        cmd.args(&config.argv[1..]);
        if let Some(dir) = &config.working_dir {
            cmd.cwd(dir);
        }
        // GUI processes often have no TERM; without one, programs assume a
        // dumb terminal and skip colors.
        if std::env::var_os("TERM").is_none() && !config.env.iter().any(|(key, _)| key == "TERM") {
            cmd.env("TERM", "xterm-256color");
        }
        for (key, value) in &config.env {
            cmd.env(key, value);
        }

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| anyhow!("Failed to spawn command in PTY: {e}"))?;
        // Close our copy of the slave end; the child holds its own.
        drop(pair.slave);

        let killer = child.clone_killer();
        #[cfg(target_family = "unix")]
        let pgid = child.process_id().map(|pid| pid as i32);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| anyhow!("Failed to clone PTY reader: {e}"))?;
        let mut writer = pair
            .master
            .take_writer()
            .map_err(|e| anyhow!("Failed to take PTY writer: {e}"))?;

        // Reader: blocking loop on a plain thread, appending into the buffer.
        let reader_buffer = buffer.clone();
        let reader_notify = output_notify.clone();
        let reader_sink = config.output_sink.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut buffer) = reader_buffer.lock() {
                            buffer.append(&chunk[..n]);
                        }
                        reader_notify.notify_one();
                        if let Some(sink) = &reader_sink {
                            sink.emit(&chunk[..n]);
                        }
                    }
                }
            }
        });

        // Writer: drains the stdin channel into the PTY master.
        let (writer_tx, mut writer_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        std::thread::spawn(move || {
            while let Some(bytes) = writer_rx.blocking_recv() {
                if writer
                    .write_all(&bytes)
                    .and_then(|_| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
        });

        // Waiter: reports the exit code (and tells the output sink, so a
        // live terminal card can mark itself finished).
        let waiter_sink = config.output_sink.clone();
        std::thread::spawn(move || {
            let code = child.wait().ok().map(|status| status.exit_code() as i32);
            let _ = exit_tx.send(Some(code));
            if let Some(sink) = &waiter_sink {
                sink.on_exit(code);
            }
        });

        Ok(Self {
            buffer,
            output_notify,
            max_buffer_bytes: config.max_buffer_bytes,
            exit_rx,
            writer_tx: Some(writer_tx),
            terminator: Mutex::new(Terminator {
                #[cfg(target_family = "unix")]
                pgid,
                killer: Some(killer),
            }),
            _master: Some(Mutex::new(pair.master)),
            _keep_alive: Mutex::new(config.keep_alive),
            started_at: Instant::now(),
            tty: true,
        })
    }

    fn spawn_piped(
        config: PtySpawnConfig,
        buffer: Arc<Mutex<HeadTailBuffer>>,
        output_notify: Arc<Notify>,
        exit_tx: watch::Sender<Option<Option<i32>>>,
        exit_rx: watch::Receiver<Option<Option<i32>>>,
    ) -> Result<Self> {
        let mut cmd = tokio::process::Command::new(&config.argv[0]);
        cmd.args(&config.argv[1..]);
        if let Some(dir) = &config.working_dir {
            cmd.current_dir(dir);
        }
        for (key, value) in &config.env {
            cmd.env(key, value);
        }
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        #[cfg(target_family = "unix")]
        cmd.process_group(0);

        let mut child = cmd.spawn().context("Failed to spawn piped command")?;
        #[cfg(target_family = "unix")]
        let pgid = child.id().map(|pid| pid as i32);

        if let Some(stdout) = child.stdout.take() {
            spawn_async_reader(
                stdout,
                buffer.clone(),
                output_notify.clone(),
                config.output_sink.clone(),
            );
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_async_reader(
                stderr,
                buffer.clone(),
                output_notify.clone(),
                config.output_sink.clone(),
            );
        }

        let waiter_sink = config.output_sink.clone();
        tokio::spawn(async move {
            let code = child.wait().await.ok().and_then(|status| status.code());
            let _ = exit_tx.send(Some(code));
            if let Some(sink) = &waiter_sink {
                sink.on_exit(code);
            }
        });

        Ok(Self {
            buffer,
            output_notify,
            max_buffer_bytes: config.max_buffer_bytes,
            exit_rx,
            writer_tx: None,
            terminator: Mutex::new(Terminator {
                #[cfg(target_family = "unix")]
                pgid,
                killer: None,
            }),
            _master: None,
            _keep_alive: Mutex::new(config.keep_alive),
            started_at: Instant::now(),
            tty: false,
        })
    }

    pub fn is_tty(&self) -> bool {
        self.tty
    }

    pub fn started_at(&self) -> Instant {
        self.started_at
    }

    pub fn status(&self) -> PtySessionStatus {
        match *self.exit_rx.borrow() {
            None => PtySessionStatus::Running,
            Some(code) => PtySessionStatus::Exited(code),
        }
    }

    /// Send bytes to the process' stdin. Only available for `tty` sessions.
    pub fn write(&self, bytes: &[u8]) -> Result<()> {
        let Some(writer_tx) = &self.writer_tx else {
            return Err(anyhow!(
                "stdin is closed for this session; start the command with tty=true to keep stdin open"
            ));
        };
        writer_tx
            .send(bytes.to_vec())
            .map_err(|_| anyhow!("The session's stdin writer has shut down"))
    }

    /// Wait up to `yield_time`, then return everything the process printed
    /// since the last collect. Returns early (after a short drain grace)
    /// when the process exits.
    pub async fn collect_output(&self, yield_time: Duration) -> CollectedOutput {
        self.collect_output_with(yield_time, |_| {}).await
    }

    /// Like [`collect_output`](Self::collect_output), but additionally
    /// forwards each raw output chunk — ANSI escape sequences included — to
    /// `on_chunk` as it arrives, so a terminal renderer can display the
    /// window live and in color. The returned `output` stays sanitized
    /// plain text.
    pub async fn collect_output_with(
        &self,
        yield_time: Duration,
        mut on_chunk: impl FnMut(&[u8]),
    ) -> CollectedOutput {
        let deadline = tokio::time::Instant::now() + yield_time;
        let mut exit_rx = self.exit_rx.clone();
        // Chunks drained during the window re-accumulate here so the
        // window's total stays capped no matter how chatty the process is.
        let mut window = HeadTailBuffer::new(self.max_buffer_bytes);
        let mut omitted_bytes = 0usize;

        loop {
            // Arm the notification before draining: an append landing
            // between drain and select stores a permit and wakes us.
            let notified = self.output_notify.notified();

            self.drain_chunk(&mut window, &mut omitted_bytes, &mut on_chunk);

            if exit_rx.borrow_and_update().is_some() {
                tokio::time::sleep(EXIT_OUTPUT_GRACE).await;
                self.drain_chunk(&mut window, &mut omitted_bytes, &mut on_chunk);
                break;
            }

            tokio::select! {
                _ = notified => {
                    // New output: loop drains and forwards it.
                }
                changed = exit_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    // Loop re-checks the exit state and applies the grace.
                }
                _ = tokio::time::sleep_until(deadline) => {
                    self.drain_chunk(&mut window, &mut omitted_bytes, &mut on_chunk);
                    break;
                }
            }
        }

        let raw = window.take_bytes();
        CollectedOutput {
            output: sanitize_terminal_output(&raw.bytes),
            omitted_bytes: omitted_bytes + raw.omitted_bytes,
            status: self.status(),
        }
    }

    /// Drain pending output, forward it to the chunk callback, and fold it
    /// into the window accumulator.
    fn drain_chunk(
        &self,
        window: &mut HeadTailBuffer,
        omitted_bytes: &mut usize,
        on_chunk: &mut impl FnMut(&[u8]),
    ) {
        let drained = self
            .buffer
            .lock()
            .map(|mut buffer| buffer.take_bytes())
            .unwrap_or_else(|_| BufferedBytes::default());
        if !drained.bytes.is_empty() {
            on_chunk(&drained.bytes);
            window.append(&drained.bytes);
        }
        *omitted_bytes += drained.omitted_bytes;
    }

    /// Ask the process to interrupt (Ctrl-C semantics).
    pub fn interrupt(&self) {
        if self.tty {
            // The PTY line discipline turns ETX into SIGINT for the
            // foreground process group.
            let _ = self.write(CTRL_C);
            return;
        }
        #[cfg(target_family = "unix")]
        if let Ok(mut terminator) = self.terminator.lock() {
            terminator.signal(libc::SIGINT);
        }
    }

    /// Kill the process (group).
    pub fn terminate(&self) {
        if self.status() != PtySessionStatus::Running {
            return;
        }
        if let Ok(mut terminator) = self.terminator.lock() {
            #[cfg(target_family = "unix")]
            terminator.signal(libc::SIGKILL);
            #[cfg(not(target_family = "unix"))]
            terminator.signal(0);
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn spawn_async_reader<R>(
    mut reader: R,
    buffer: Arc<Mutex<HeadTailBuffer>>,
    notify: Arc<Notify>,
    sink: Option<Arc<dyn TerminalOutputSink>>,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt as _;
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut buffer) = buffer.lock() {
                        buffer.append(&chunk[..n]);
                    }
                    notify.notify_one();
                    if let Some(sink) = &sink {
                        sink.emit(&chunk[..n]);
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(command_line: &str, tty: bool) -> PtySpawnConfig {
        let mut config = PtySpawnConfig::shell_command(command_line);
        config.tty = tty;
        config
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tty_command_runs_to_completion() {
        let session = PtySession::spawn(shell("echo hello-pty", true)).unwrap();
        let out = session.collect_output(Duration::from_secs(10)).await;
        assert!(out.output.contains("hello-pty"), "output: {}", out.output);
        assert_eq!(out.status, PtySessionStatus::Exited(Some(0)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn exit_code_is_reported() {
        let session = PtySession::spawn(shell("exit 7", true)).unwrap();
        let out = session.collect_output(Duration::from_secs(10)).await;
        assert_eq!(out.status, PtySessionStatus::Exited(Some(7)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn long_running_command_yields_while_running() {
        let session = PtySession::spawn(shell("echo started; sleep 30", true)).unwrap();
        let out = session.collect_output(Duration::from_millis(500)).await;
        assert!(out.output.contains("started"), "output: {}", out.output);
        assert_eq!(out.status, PtySessionStatus::Running);

        session.terminate();
        let out = session.collect_output(Duration::from_secs(10)).await;
        assert!(matches!(out.status, PtySessionStatus::Exited(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tty_session_is_interactive() {
        let session = PtySession::spawn(shell("cat", true)).unwrap();
        session.write(b"marco-polo\n").unwrap();
        let out = session.collect_output(Duration::from_secs(1)).await;
        // The PTY echoes input, and cat repeats it.
        assert!(out.output.contains("marco-polo"), "output: {}", out.output);
        assert_eq!(out.status, PtySessionStatus::Running);
        session.terminate();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn piped_session_captures_stderr_and_rejects_stdin() {
        let session = PtySession::spawn(shell("echo err-marker 1>&2; sleep 30", false)).unwrap();
        assert!(session.write(b"nope\n").is_err());
        let out = session.collect_output(Duration::from_secs(1)).await;
        assert!(out.output.contains("err-marker"), "output: {}", out.output);
        session.terminate();
        let out = session.collect_output(Duration::from_secs(10)).await;
        assert!(matches!(out.status, PtySessionStatus::Exited(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn collect_after_exit_returns_immediately() {
        let session = PtySession::spawn(shell("true", true)).unwrap();
        let _ = session.collect_output(Duration::from_secs(10)).await;

        let started = Instant::now();
        let out = session.collect_output(Duration::from_secs(30)).await;
        assert!(matches!(out.status, PtySessionStatus::Exited(_)));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "collect after exit should not wait for the yield time"
        );
    }

    #[test]
    fn sanitize_strips_ansi_and_normalizes_line_endings() {
        let text = sanitize_terminal_output(b"\x1b[31mred\x1b[0m\r\nplain\rprogress");
        assert_eq!(text, "red\nplain\nprogress");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn streaming_chunks_arrive_incrementally() {
        let session = PtySession::spawn(shell(
            "printf 'first\\n'; sleep 0.5; printf 'second\\n'",
            true,
        ))
        .unwrap();

        let start = Instant::now();
        let mut chunks: Vec<(Duration, String)> = Vec::new();
        let out = session
            .collect_output_with(Duration::from_secs(10), |bytes| {
                chunks.push((start.elapsed(), String::from_utf8_lossy(bytes).into_owned()));
            })
            .await;

        assert!(matches!(out.status, PtySessionStatus::Exited(_)));
        let combined: String = chunks.iter().map(|(_, text)| text.as_str()).collect();
        assert!(combined.contains("first"), "chunks: {combined:?}");
        assert!(combined.contains("second"), "chunks: {combined:?}");

        let first_seen = chunks
            .iter()
            .find(|(_, text)| text.contains("first"))
            .map(|(at, _)| *at)
            .expect("'first' should have been streamed");
        assert!(
            first_seen < Duration::from_millis(400),
            "'first' should stream before the process finishes, got {first_seen:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn result_is_plain_text_while_chunks_stay_raw() {
        let session =
            PtySession::spawn(shell("printf '\\033[31mcolored\\033[0m\\n'", true)).unwrap();

        let mut raw = Vec::new();
        let out = session
            .collect_output_with(Duration::from_secs(10), |bytes| {
                raw.extend_from_slice(bytes);
            })
            .await;

        assert!(out.output.contains("colored"), "output: {}", out.output);
        assert!(
            !out.output.contains('\u{1b}'),
            "result should be ANSI-free: {:?}",
            out.output
        );
        assert!(
            raw.contains(&0x1b),
            "raw chunks should keep escape sequences"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn output_sink_receives_all_raw_chunks() {
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct RecordingSink(Mutex<Vec<u8>>);
        impl TerminalOutputSink for RecordingSink {
            fn emit(&self, bytes: &[u8]) {
                self.0.lock().unwrap().extend_from_slice(bytes);
            }
        }

        let sink = Arc::new(RecordingSink::default());
        let mut config = shell("printf '\\033[36mcyan\\033[0m\\n'", true);
        config.output_sink = Some(sink.clone());

        let session = PtySession::spawn(config).unwrap();
        let _ = session.collect_output(Duration::from_secs(10)).await;

        let raw = sink.0.lock().unwrap().clone();
        assert!(
            String::from_utf8_lossy(&raw).contains("cyan"),
            "sink should have received the output"
        );
        assert!(raw.contains(&0x1b), "sink should receive raw ANSI escapes");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn interrupt_stops_a_tty_command() {
        let session = PtySession::spawn(shell("sleep 30", true)).unwrap();
        let out = session.collect_output(Duration::from_millis(300)).await;
        assert_eq!(out.status, PtySessionStatus::Running);

        session.interrupt();
        let out = session.collect_output(Duration::from_secs(10)).await;
        assert!(matches!(out.status, PtySessionStatus::Exited(_)));
    }
}
