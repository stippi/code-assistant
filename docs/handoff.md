# New context and handoff

Status: implemented (2026-09-30).

Two slash commands end the current model context on purpose:

- `/new [prompt]` starts a fresh context whose first user message is
  `prompt`.
- `/handoff [instruction]` (alias `/compact`) first lets the agent write a
  handoff prompt from the current context, then continues exactly like
  `/new <that prompt>`.

Both ask where the fresh context lives: behind a divider in the same session,
or in a new session of the same project. They are the user-initiated
siblings of context compaction (`docs/context-compaction.md`) and reuse its
boundary machinery.

When a long session goes idle, the core writes a handoff prompt while the
prompt cache is still warm and offers it in the composer as `/new <prompt>`.
The user can read, edit and send it; no second model request is needed.

## Recognition

- A message starting with `/new`, `/handoff` or `/compact` is recognized in
  `SessionService` (send, send-or-queue), before the skill trigger. Skills
  with these names are shadowed: the slash menus list the built-in entries
  only.
- Accepting a slash-menu entry inserts `/<command> ` so the argument can be
  typed right after it (like skills).
- Only accepted while the session is idle; during a run the message is
  rejected with an error (not queued). Attachments are rejected too.
- Editing a message into `/handoff` branches off like any edit; the
  handoff is written from that branch. `/new` cannot replace an edited
  message: it stores no message, and its boundary is appended at the end
  of the active path.

## Target choice

Both run as a new-context run (`RunTask::NewContext`,
`session/new_context.rs`), so the session shows as running and Stop
cancels. The run asks inline where to continue: *This session* / *New
session*. Transport mirrors permission prompts:
`UiEvent::RequestNewContextTarget` +
`SessionService::respond_new_context_target`, the open request is part of
snapshots, Stop drops it. For `/handoff` the question runs concurrently
with the generation, so the user does not wait twice.

## `/new [prompt]`

- **Same session**: a boundary node is appended (see Storage) and the UI
  shows a divider labelled *New context*; expanding it shows the prompt. If
  a prompt was given, the agent starts on it; otherwise the next message the
  user sends is the first message of the new context.
- **New session**: a session with the old session's config (project,
  worktree, model, sandbox, permission tier — `start_fresh_session`) is
  created. The prompt becomes its first user message, shown as if the user
  had typed it, and the agent starts on it. The old session is left
  untouched. `UiEvent::SessionHandedOff { from, to }` lets a frontend viewing
  `from` switch to `to`. Without a prompt this is the same as `/clear`.

## `/handoff [instruction]`

1. The `/handoff …` message is stored as a user message. The handoff
   prompt (`resources/handoff_prompt.md`, including the user's instruction)
   is appended to it as a hidden block, the same way an invoked skill's body
   is (`skills::trigger`). Transcript, edit context and pending-message
   summary hide the block. The instruction only steers the agent writing
   the handoff; the new context sees only the generated prompt.
2. A handoff run starts. `AgentRuntime::generate_handoff` sends the history
   as it is (it already ends with the request), non-streaming, with tools
   and system prompt unchanged, so the whole prefix is a cache read. It
   shares the retry-on-tool-calls logic with compaction's `request_handoff`.
   The target question runs meanwhile.
3. **Same session**: as `/new <generated prompt>` — boundary node, divider,
   the agent continues from the new context within the same run.
4. **New session**: the generated prompt is stored in the old session as the
   assistant's answer to the `/handoff` message (its history stays
   well-formed and shows what was handed over); then as `/new <generated
   prompt>` in a new session. The run hands the prompt back over a oneshot;
   a task holding the service creates the session once the run is done
   (`session/service/new_context.rs`).
5. **Stopped or failed** before the new context opened: the `/handoff`
   message is removed from the history again (an edit it replaced becomes
   the active branch again) and frontends get the transcript without it.
   The target question settles on every exit, so prompts are dismissed.

The handoff prompt asks the model to write a self-contained prompt for a
fresh instance, one that reads as the opening message of a new session, and
to carry user preferences and constraints over explicitly, since none of the
user's earlier messages are handed over.

## Storage

The boundary node keeps `is_compaction_summary` (so everything that cuts the
prompt at the last summary keeps working) and gets a new `is_new_context`
flag (`#[serde(default)]`, so existing sessions load unchanged). Its content
is the prompt. `prompt_messages` sends it as a plain user message — no
`<handoff>` wrapper, no earlier user messages. An empty boundary (`/new`
without prompt) is left out of the prompt. User messages directly after
the opening message of a context (this prompt, or a compaction handoff),
such as one queued while it was written, are folded into it in the request,
so it never starts with consecutive user messages.

## Prepared handoff (idle)

- **Arming**: when a run ends in which the agent answered in the session,
  the core arms a per-session deadline of 2 minutes. A `/new` that only
  opened an empty context or moved to a new session does not arm it.
- **User activity**: frontends report typing in the viewed session via
  `SessionService::note_user_activity(session_id)`, throttled per session
  (`ActivityThrottle`); it pushes the deadline back. Sessions in the
  background count as inactive.
- **Firing**: the session qualifies when it is idle (not errored), its
  current context (after the last compaction or new context) ends with the
  agent's answer, the request for that answer had at least the threshold
  of input (input + cache write + cache read), and nothing was prepared
  for this state yet (`SessionManager::claim_handoff_preparation`). Then a
  `RunTask::PrepareHandoff` run sends history + one appended user message
  holding the idle request. It tells the agent that this is an automatic
  handoff because the user is inactive and the prompt cache expires soon,
  that the system cannot tell whether a handoff makes sense right now, and
  that it may decline by replying `[cancel handoff]` (also recognized with
  backticks, quotes, a period or without brackets). It holds the run like
  any other, so the session shows as running meanwhile, but nothing is
  written to the transcript. A declined or failed preparation offers
  nothing, is not retried for the same state and does not mark the session
  errored; a message queued meanwhile is answered instead (which re-arms
  the timer) and the prompt is dropped.
- **Result**: published as `UiEvent::HandoffPrepared { prompt }` for the session.
  A frontend puts `/new <prompt>` into that session's composer if it is
  empty; a non-empty draft is never overwritten.
- **Setting**: the threshold (default 150 000 tokens, 0 disables) lives in
  the core configuration so GPUI and the terminal share it; GPUI edits it in
  Settings → General.

The timers live in the core (`session/idle_handoff.rs`, installed by the
wiring layers like the wakeup scheduler), so several frontends on one
session never prepare twice.

## Frontends

- **GPUI**: slash-menu entries; target choice as an inline banner like the
  permission banner; *New context* divider; switch on `SessionHandedOff`;
  fill an empty composer or draft on `HandoffPrepared`; report composer
  activity; threshold in Settings → General.
- **Terminal**: command-list entries; lines starting with the commands are
  sent as messages; target choice as a modal prompt like the permission
  prompt (Esc stops the command); divider; switch; fill the current
  session's empty composer; activity on keystrokes.
- **ACP**: out of scope (ACP bypasses `SessionService`); follow-up.

## Known gaps

- ACP bypasses `SessionService`, so the commands are not recognized there.
- `/new` cannot replace an edited message (the run would have to branch
  the tree itself and publish branch info).
- The terminal only fills the current session's composer; a handoff
  prepared for a background session is not kept for later. A retracted
  `/handoff` message stays visible in its scrollback.
- A message queued while the target question is open and then answered
  with *New session* stays pending in the old session.
