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

1. Build the wakeup message: a user-role message with unambiguous framing,
   `[scheduled wakeup] {prompt}` (constant `WAKEUP_PREFIX`), so the agent
   and the transcript can tell it from a human message.
2. If the session is idle: `add_user_message` + `start_agent_for_session`
   (the `send_user_message_impl` path).
3. If a turn is running: write into the `pending_message` slot
   (`queue_structured_user_message` path) — the running turn picks it up at
   its next iteration; no second task is spawned.
4. If the session no longer exists: drop silently.

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

`cancel_wakeup` — input: wakeup id. Cancels if still armed.

The tool reaches the scheduler handle via the `ToolContext` extensions
(same access pattern as `update_plan` reaching app services).

### Interactions

- **SleepInhibitor**: an armed wakeup holds the wake-lock refcount (armed →
  `agent_started`-equivalent acquire, fired/cancelled → release), otherwise
  the host may sleep through the deadline. Refcounted exactly like running
  agents.
- **Session deletion**: `SessionManager` cancels the session's wakeups
  (`CancelSession`) when a session is deleted.
- **Stop-requested / errored sessions**: firing into an errored session
  behaves like a user message would (it may un-stick it); no special case.

### Future: condition-based monitors

A monitor is just another producer of the same fire path: a background
watcher evaluates a condition and, when it holds, injects a framed message
and starts a turn. Candidates: background-command exit, file-change
patterns (reusing the `notify` machinery from `SessionWatcher`), log
markers. Out of scope for v1; the fire path and cancellation semantics
above are the contract they will reuse.

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
