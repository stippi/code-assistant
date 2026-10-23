# Background monitors and job notifications

The agent can start work in the background, continue with other things (or
end its turn), and be told when there is something to look at — instead of
polling with `write_stdin` or arming wakeups to check back.

Two producers share one delivery path, the one wakeups use
(`docs/session-wakeups.md`): a framed user-role message is injected into the
session; when the session is idle it starts a turn, while a turn runs it is
queued as the pending message and picked up at the next iteration.

Code: `code_assistant_core::session::monitor` (supervision, framing,
delivery), `tools::impls::monitor` (tools), `tools::impls::execute_command`
(job exits).

## Monitors (`monitor`, `stop_monitor`)

`monitor(project, command_line, description, working_dir?, timeout_seconds?,
ask_user_approval?)` runs a command (sandboxed like `execute_command`, plain
pipes, stdin closed). **Every line it prints is an event.** Typical uses:

```sh
# Tell me when the CI status of PR 42 changes
prev=""; while true; do
  s=$(gh pr checks 42 2>/dev/null | awk '{print $2}' | sort | uniq -c | tr '\n' ' ')
  [ "$s" != "$prev" ] && echo "checks: $s"; prev="$s"; sleep 60
done
```

```sh
tail -n0 -f server.log | grep --line-buffered -E 'ERROR|panic'
```

Messages:

- `[monitor #3] <description>` followed by the lines. Lines arriving within
  500 ms of the first are delivered together; at most 40 lines per message
  (more are counted as omitted), long lines are cut at 400 characters.
- `[monitor #3 ended] <description>` + why (exit code, timeout, flood guard)
  + the last lines, when the command ends on its own or is stopped by the
  timeout (default 1 h, max 7 days) or the flood guard.
- Nothing when it is stopped with `stop_monitor` (or the user's *Stop*
  button, or the session is deleted) — the command is terminated.

Flood guard: a monitor delivering more than 20 messages within a minute is
stopped (each message may start a model turn), and told to print only on
changes.

Lifetime: like wakeups, monitors are in-memory and die with the process;
ids are numbered per session and continue the session's history
(`highest_monitor_id`). Sub-agents get no monitors (they would be gone when
events arrive). A running monitor holds the sleep inhibitor and keeps its
session from settling automatically.

## Background job exits (`execute_command` session mode)

When `execute_command` returns while the process still runs
(`pty_session_id`), `SessionMonitors::watch_job_exit` waits for the exit
(`PtySession::exited`, which holds no reference to the process). One second
after it, if the job is still tracked by the session's `PtySessionManager`,
the agent gets:

```
[background job 12345 exited] cargo build --release
It exited with code 0. Read its remaining output with write_stdin (session_id 12345) if you need it.
```

A `write_stdin` call that sees the exit stops tracking the job, so a job the
agent watched exit itself is not reported again; neither is a job evicted
from the registry or terminated with its session. The tool result tells the
model that the notification will come (`notify_on_exit`), so it does not
poll.

## Frontends

- `UiEvent::UpdateActiveMonitors` (also in the session snapshot,
  `active_monitors`) lists the running monitors.
- GPUI: the sidebar shows an idle session with a monitor as `Scheduled`
  ("Monitoring"); below the transcript, each monitor gets a line with a
  *Stop* button (`SessionService::stop_monitor`). Notification messages
  render as compact lines (`parse_notification_messages`) instead of user
  bubbles.
- Wiring: frontends install `MonitorHandle::new(service, inhibitor)` via
  `SessionManager::set_monitor_handle` next to the wakeup scheduler; without
  it the monitor tools report themselves unavailable and job exits are not
  announced.
