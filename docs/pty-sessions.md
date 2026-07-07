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
