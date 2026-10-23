# Session-scoped wakeups

Status: design accepted, implementation in progress (2026-07-06).

## Motivation

An agent can currently only act when a user message (or `resume_session`)
starts a turn. There is no way for the agent to say "wake me in 20 minutes
and I'll check the build" or, later, "wake me when this condition holds".
Claude Code demonstrates the value of this primitive (its `ScheduleWakeup`
and `Monitor` tools): an agent that can schedule its own continuation can
self-orchestrate — kick off long-running work, go idle, and resume when
there is something to do.

Downstream, pal needs the same seam: its gateway wants to start turns
without a user message (scheduled jobs, IMAP IDLE events). The wakeup
mechanism is the generic, session-scoped foundation; durable cross-session
scheduling stays application-level (pal persists jobs itself, because its
sessions are deliberately short-lived).

## Scope and lifetime model

- **Session-scoped**: a wakeup belongs to one session. It is cancelled when
  the session is deleted. It is *not* persisted — process restart drops all
  armed wakeups. Durable scheduling is an application concern (see pal).
- **Time-based first**: v1 is `schedule_wakeup(delay_seconds, prompt)`.
  Condition-based monitors (file changes, command exit, log markers) are a
  later producer feeding the same fire path; the seam is designed so they
  slot in without new session plumbing.

## Existing architecture this builds on

The runtime loop (`agent_core::AgentRuntime::run_single_iteration`) returns
on `LoopFlow::GetUserInput`; the session then sits in
`SessionManager::active_sessions` as an idle `SessionInstance` with **no
task running**. The next user message spawns a fresh turn task
(`start_agent_for_session`). `resume_session` already starts a turn against
existing history without a new user message.

So "idle-but-alive" already exists. A wakeup only needs a producer that,
at fire time, injects a message and starts a turn — exactly the path
`send_user_message` / `resume_session` take through the `SessionService`
command bus.

## Design

### WakeupScheduler

New module `code_assistant_core::session::wakeup`.

```rust
pub struct Wakeup {
    pub id: WakeupId,          // monotonic per process
    pub session_id: String,
    pub fire_at: SystemTime,
    pub prompt: String,        // what the agent asked to be told
}
```

`WakeupScheduler` is a single background tokio task owning a min-heap of
armed wakeups. It receives commands over an mpsc channel:

- `Arm(Wakeup)` — insert, recompute sleep deadline.
- `Cancel(WakeupId)` / `CancelSession(session_id)` — remove.

The task loops on `tokio::select!` over the command channel and
`sleep_until(next_deadline)`. No per-wakeup task spawns; one heap, one
timer. This mirrors the `SessionWatcher` pattern (single observing task
emitting into the core) rather than hermes' 60s polling tick — with a heap
there is no reason to poll.

The scheduler holds a handle to fire wakeups (see below) — wired the same
way other cross-component handles are, at service construction time.

### Fire path

On fire, the scheduler goes through the `SessionService` command bus:

1. Record in the stored `schedule_wakeup` result that it fired
   (`fired_at`, `SessionManager::mark_wakeup_fired`) and send the result's
   update to frontends.
2. Build the wakeup message: a user-role message with unambiguous framing,
   `[scheduled wakeup #{id}] {prompt}` (`wakeup_message`), so the agent and
   the transcript can tell it from a human message.
3. If the session is idle: `add_user_message` + `start_agent_for_session`
   (the `send_user_message_impl` path).
4. If a turn is running: write into the `pending_message` slot
   (`queue_structured_user_message` path) — the running turn picks it up at
   its next iteration; no second task is spawned.
5. If the session no longer exists: drop silently.

The turn then ends naturally (`LoopFlow::GetUserInput` → task returns →
`Idle`), keeping the session idle-but-alive for the next wakeup.

### Tools

`schedule_wakeup` — registered in `register_default_tools`, offered in the
main agent scopes. Sub-agents don't get it in v1: they run to completion
inside the parent's turn, so a wakeup for "their" session would in effect
wake the parent — if that turns out useful it needs its own semantics.

- Input: `delay_seconds: u64` (clamped to a sane range), `prompt: String`.
- Effect: arms a wakeup for the calling session (session id from
  `ToolContext`), returns the wakeup id and resolved fire time.
- Ids are numbered per session and continue across restarts (seeded from
  the highest id in the session's history), so the model sees short ids;
  internally a wakeup is identified by session and id.
- The result records the process that armed it (`armed_by`,
  `process_instance()`). A frontend shows a wakeup armed by another process
  that never fired as lost with a restart. The text the model sees leaves
  out `fired_at`, so the conversation never changes after the fact.

`cancel_wakeup` — input: wakeup id. Cancels if still armed.

The tool reaches the scheduler handle via the `ToolContext` extensions
(same access pattern as `update_plan` reaching app services).

### Frontends

The scheduler keeps, per session, the wakeups still to fire
(`WakeupHandle::armed`) and reports every change
(`WakeupSink::armed_changed` → `UiEvent::UpdateArmedWakeups`, also part of
the session snapshot). The GPUI frontend shows an idle session with one as
`SessionStatus::Scheduled` in the sidebar and a countdown line below the
transcript, so a session that goes on later does not look finished. Such a
session never settles automatically.

### Interactions

- **SleepInhibitor**: an armed wakeup holds the wake-lock refcount (armed →
  `agent_started`-equivalent acquire, fired/cancelled → release), otherwise
  the host may sleep through the deadline. Refcounted exactly like running
  agents.
- **Session deletion**: `SessionManager` cancels the session's wakeups
  (`CancelSession`) when a session is deleted.
- **Stop-requested / errored sessions**: firing into an errored session
  behaves like a user message would (it may un-stick it); no special case.

### Condition-based monitors

Implemented as another producer of the same inject-or-queue path: the
`monitor` tool (a command whose output lines are events) and exit
notifications for background `execute_command` jobs. See
`docs/background-monitors.md`.

## Downstream: pal's durable scheduler (for context)

pal cannot rely on session-scoped wakeups for reminders: its lanes rotate
incarnations on idle/daily reset, so "remind me tomorrow" must outlive the
session. pal therefore keeps a persistent job store (`$PAL_HOME/jobs.json`;
once / interval / daily jobs) and a gateway pass (sibling of its expiry
watcher) that resolves the lane's current session and starts a turn through
its backend seam — the same inject-and-run primitive, driven from
application code instead of the in-process scheduler. Session-scoped
wakeups and pal's jobs compose: a pal job fires, the woken agent may arm
short-lived wakeups within its incarnation to follow up on its own work.
