# New context and hand-off

Status: design accepted, implementation in progress (2026-09-30).

Two slash commands end the current model context on purpose:

- `/new [prompt]` starts a fresh context whose first user message is
  `prompt`.
- `/hand-off [instruction]` (alias `/compact`) first lets the agent write a
  hand-off prompt from the current context, then continues exactly like
  `/new <that prompt>`.

Both ask where the fresh context lives: behind a divider in the same session,
or in a new session of the same project. They are the user-initiated
siblings of context compaction (`docs/context-compaction.md`) and reuse its
boundary machinery.

When a long session goes idle, the core writes a hand-off prompt while the
prompt cache is still warm and offers it in the composer as `/new <prompt>`.
The user can read, edit and send it; no second model request is needed.

## Recognition

- A message starting with `/new`, `/hand-off` or `/compact` is recognized in
  `SessionService` (send, send-or-queue), before the skill trigger. Skills
  with these names are shadowed: the slash menus list the built-in entries
  only.
- Accepting a slash-menu entry inserts `/<command> ` so the argument can be
  typed right after it (like skills).
- Only accepted while the session is idle; during a run the message is
  rejected with an error (not queued). Attachments are rejected too.

## Target choice

After such a message the core asks inline where to continue: *Continue in
this session* / *Start a new session*. Transport mirrors permission
prompts: `UiEvent::RequestNewContextTarget` +
`SessionService::respond_new_context_target`, open requests included in
snapshots, Stop cancels the command. For `/hand-off` the question runs
concurrently with the generation, so the user does not wait twice.

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

## `/hand-off [instruction]`

1. The `/hand-off …` message is stored as a user message. The hand-off
   prompt (`resources/handoff_prompt.md`, including the user's instruction)
   is appended to it as a hidden block, the same way an invoked skill's body
   is (`skills::trigger`). Transcript, edit context and pending-message
   summary hide the block. The instruction only steers the agent writing
   the hand-off; the new context sees only the generated prompt.
2. A hand-off run starts. `AgentRuntime::generate_handoff` sends the history
   as it is (it already ends with the request), non-streaming, with tools
   and system prompt unchanged, so the whole prefix is a cache read. It
   shares the retry-on-tool-calls logic with compaction's `request_handoff`.
   The target question runs meanwhile.
3. **Same session**: as `/new <generated prompt>` — boundary node, divider,
   the agent continues from the new context within the same run.
4. **New session**: the generated prompt is stored in the old session as the
   assistant's answer to the `/hand-off` message (its history stays
   well-formed and shows what was handed over); then as `/new <generated
   prompt>` in a new session. The run reaches the service through a
   `NewContextSink` (implemented by `SessionService`, like `WakeupSink`).

The hand-off prompt asks the model to write a self-contained prompt for a
fresh instance, one that reads as the opening message of a new session, and
to carry user preferences and constraints over explicitly, since none of the
user's earlier messages are handed over.

## Storage

The boundary node keeps `is_compaction_summary` (so everything that cuts the
prompt at the last summary keeps working) and gets a new `is_new_context`
flag (`#[serde(default)]`, so existing sessions load unchanged). Its content
is the prompt. `prompt_messages` sends it as a plain user message — no
`<handoff>` wrapper, no earlier user messages. An empty boundary (`/new`
without prompt) is left out of the prompt.

## Prepared hand-off (idle)

- **Arming**: when a run ends with the session idle and the last request's
  input (input + cache write + cache read) exceeds the threshold, the core
  arms a per-session deadline of 2 minutes.
- **User activity**: frontends report typing in a session's composer via
  `SessionService::note_user_activity(session_id)` (debounced); it pushes
  the deadline back. Sending a message disarms it.
- **Firing**: if the session is still idle and nothing was prepared for its
  current head, the core sends a side request: history + one appended user
  message holding the idle prompt ("the user is away; describe the most
  obvious next step for a follow-up session as a hand-off prompt"). Nothing
  is written to the transcript and no run is reserved; a message arriving
  meanwhile starts a normal run and the result is dropped.
- **Result**: published as `UiEvent::HandoffPrepared { session_id, prompt }`.
  A frontend puts `/new <prompt>` into that session's composer if it is
  empty; a non-empty draft is never overwritten.
- **Setting**: the threshold (default 150 000 tokens, 0 disables) lives in
  the core configuration so GPUI and the terminal share it; GPUI edits it in
  Settings → General.

The scheduler is one tokio task with a deadline per session, like the
`WakeupScheduler`, so several frontends on one session never prepare twice.

## Frontends

- **GPUI**: slash-menu entries; target choice as an inline banner like the
  permission banner; *New context* divider; switch on `SessionHandedOff`;
  fill an empty composer or draft on `HandoffPrepared`; report composer
  activity; threshold in Settings → General.
- **Terminal**: command-list entries; lines starting with the commands are
  sent as messages (the `CompactContext` command goes away); target choice
  as a modal prompt like the permission prompt; divider; switch; fill the
  current session's empty composer; activity on keystrokes.
- **ACP**: out of scope (ACP bypasses `SessionService`); follow-up.

## Implementation steps

1. `agent_core`: `is_new_context` boundary in the prompt projection,
   `generate_handoff`, appending a boundary.
2. `code_assistant_core`: command recognition, target choice, `/new` in both
   targets, `SessionHandedOff`; remove the `compact_context` stub.
3. `/hand-off` run: hidden prompt block, generation, both targets.
4. Prepared hand-off: idle scheduler, activity reporting, setting.
5. GPUI.
6. Terminal.
