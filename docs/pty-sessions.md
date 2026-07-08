# PTY Sessions (interactive & background commands)

`execute_command` classically blocks until the child exits and returns all
output at once — unusable for interactive programs and long-running
processes. PTY sessions add a second mode in which a process outlives the
tool call: the agent gets an output window plus a `session_id` and keeps
interacting through the `write_stdin` tool. The design follows Codex's
"unified exec" (`exec_command` / `write_stdin`).

## Model-facing behavior

- `execute_command` with `tty` and/or `yield_time_ms` set runs the command
  in **session mode**: the tool waits at most `yield_time_ms`
  (250–30000 ms, default 10000) and returns the output produced so far.
  - Process exited in time → result carries the exit code, nothing is
    tracked.
  - Process still running → result carries a `session_id`; the process
    keeps running in the background.
- `tty: true` (the default in session mode) allocates a real PTY with a
  controlling terminal — ssh logins, sudo prompts, and REPLs work, and
  stdin stays open. `tty: false` uses plain pipes with stdin closed, for
  non-interactive background processes.
- `write_stdin(session_id, chars, yield_time_ms)` sends `chars` verbatim
  to the session and returns the output since the last call. Empty
  `chars` polls without writing (default yield 10 s, max 300 s — for
  watching slow builds/servers). `"\u0003"` interrupts (Ctrl-C), which
  also works for pipe sessions (SIGINT to the process group).

## Implementation

- **`crates/pty_session`** (Layer 0, UI-free): `PtySession` spawns via
  `portable-pty` (tty) or `tokio::process` (pipes); both paths make the
  child a process-group leader so terminate/interrupt reap descendants.
  Output accumulates between reads in a `HeadTailBuffer` (1 MiB cap,
  keeps head + tail, counts omitted middle bytes). `collect_output`
  waits for the yield window (early return + drain grace on exit).
- **`PtySessionManager`**: id-keyed registry (random ids, LRU cap of 32,
  prefers evicting exited sessions). One per `SessionInstance`, handed
  to tools via `ToolServices`; dropping the manager terminates all
  sessions, so PTY sessions survive across agent runs but die with the
  agent session. Sub-agents get a private manager that is dropped when
  the sub-agent completes.
- **Sandboxing**: `CommandExecutor::prepare_pty_spawn` returns the argv
  to spawn. `SandboxedCommandExecutor` wraps it in a seatbelt invocation
  (macOS); the profile temp file travels as a `keep_alive` guard on the
  session. `ask_user_approval` bypasses the sandbox as in classic mode.
- Environments without a session (e.g. MCP server mode) have no
  registry; session mode fails there with a clear error.

## Live colored output (GPUI)

Terminal output flows one-way from the backend to the frontends — the
agent loop never waits on a UI thread (the old GPUI terminal-worker
round-trip, a recurring source of stalls, is gone):

- A `PtySession` can carry a `TerminalOutputSink`, bound at spawn time,
  that receives every raw output chunk (ANSI escapes included) for the
  session's **whole lifetime** — including between turns, so a background
  process keeps streaming live colored output to its card while the agent
  does other work. `execute_command` session mode binds this sink (via
  `UserInterface::stream_terminal_output`, which publishes
  `DisplayFragment::ToolTerminalOutput` straight onto the broadcast
  stream, **bypassing** the in-flight fragment buffer so background
  streaming doesn't grow snapshots). `write_stdin` reactions surface on
  the same original card through the same sink.
- Alongside the live raw stream, the polling tools emit one sanitized
  plain `ToolOutput` chunk per window (the model result and what text
  frontends render). Text frontends (TUI, ACP) ignore the raw variant.
- `PtySession::collect_output_with` (used by the classic blocking
  `PtyCommandExecutor`) forwards raw chunks per poll window instead; the
  returned result text is always sanitized (escapes stripped, CR/CRLF
  normalized) for the LLM.
- GPUI feeds the raw bytes into a display-only alacritty terminal in the
  `TerminalPool` (keyed by tool_id); the terminal card picks it up and
  renders live colored output. `Terminal::write_output` keeps a
  persistent vte parser so escape sequences split across chunks parse
  correctly. A cap (32) evicts the oldest display terminals into the
  styled-output cache, which the card uses for static colored rendering.
- Classic blocking commands in GPUI run through
  `command_executor::PtyCommandExecutor` (backend PTY, 5-minute
  timeout), streaming the same way via
  `StreamingCallback::on_terminal_output_chunk`.
- Process exit is propagated to the display-only terminal so the card can
  stop the running spinner/stop button (the display terminal has no PTY
  event loop, so it never learns of the exit on its own). Both transports
  signal it: session mode via `TerminalOutputSink::on_exit` (driven from
  `PtySession`'s exit waiter, so even a background process the agent never
  polls again updates its card), classic blocking via
  `StreamingCallback::on_terminal_exit`. Both funnel through
  `UserInterface::stream_terminal_exit` → `DisplayFragment::ToolTerminalExited`
  (published directly, bypassing the in-flight buffer like the raw output),
  which GPUI turns into `Terminal::set_exit_status`.
- The terminal card's **stop button** interrupts the real backend process
  (the pool terminal is display-only, so writing to it is a no-op). The
  click routes through `SessionService::interrupt_terminal(session_id,
  tool_id)`, which sends Ctrl-C to a background `PtySession` via
  `PtySessionManager::interrupt_by_tool_id`, or — for a classic blocking
  command whose PTY lives in the executor — sets a per-tool cancel flag
  (`TerminalInterrupts`, on the session instance) that the executor polls
  via `StreamingCallback::should_continue` and then interrupts.
