# Voice mode

Status: implemented (2026-10-08). Code: `code_assistant_core::voice`,
`llm::realtime`, `crates/audio`, GPUI title bar and *Settings → Voice*.

## Goal

A global voice assistant, separate from any single session. The user
switches it on once (title bar) and then talks to it about all of their
conversations: "what is running?", "start a conversation in project X that
fixes the flaky test", "tell the refactoring session to also update the
docs", "what did the build session end with?".

- **Global, not per session.** One voice agent per app process. It never
  lives inside a session's transcript and is not tied to the session being
  viewed.
- **Realtime model, configured in settings.** A speech-to-speech model
  (OpenAI Realtime protocol first) chosen in a new *Voice* settings section.
- **Its own small agent loop.** It has no file, shell or browser tools.
  Its only tools act on conversations, through `SessionService`.
- **Voice first.** When a background conversation finishes, the voice agent
  is told, but the notification never interrupts the user or the voice
  agent while either is speaking. It waits for real silence.

Non-goals for v1: voice input into a normal session's composer (dictation),
answering permission prompts or questions by voice (see Follow-ups), the
terminal and ACP frontends.

## Why not `agent_core::runtime`

The existing agent loop is request/response: build the history, call
`LLMProvider::send_message`, run the tools, repeat. A realtime model works
differently. One long-lived WebSocket session keeps the conversation state
on the server, audio streams both ways all the time, the server detects
turns (VAD), and a response can be cancelled halfway through playback. So
the voice agent gets its own event loop. It reuses:

- `SessionService` and `EventStream`, as its only link to the rest of the
  app, exactly like a frontend;
- `session_query` — for reading transcripts.

Its four tools are a small dispatch of their own (`voice::tools`), not a
`tools_core` registry: they need none of the tool context (command executor,
permissions, rendering) and are declared to the realtime session as plain
function schemas.

## Architecture

```
            ┌──────────── ui_gpui ─────────────┐
            │ title-bar toggle, voice indicator │
            │ Voice settings section           │
            └──────▲──────────────┬────────────┘
       UiEvent::Voice*   VoiceService::start/stop
                   │              │
┌──────────────── code_assistant_core::voice ─────────────────┐
│ VoiceService (start/stop, owns one VoiceAgent task)         │
│                                                             │
│ VoiceAgent event loop ── select! over:                      │
│   • realtime server events      (RealtimeConnection)        │
│   • audio events                (captured, drained)         │
│   • tool completions            (spawned tool futures)      │
│   • EventStream subscription  → NotificationSource          │
│   • timers                      (cooling, playback fallback)│
│ every input → Floor::handle(event) → Vec<FloorCommand>      │
│                                                             │
│ Floor (pure state machine)   NotificationQueue   voice tools │
└───────▲───────────────────────────────▲──────────────────────┘
        │                               │
  llm::realtime (protocol,         audio (voice-processing I/O on
  WebSocket transport)              macOS, cpal elsewhere)
```

### New pieces by layer

| Layer | Piece | Responsibility |
|---|---|---|
| 0 | `llm::realtime` | Protocol types (`ClientEvent`, `ServerEvent`), `WsConnector` over `tokio-tungstenite`, `RealtimeEndpoint::openai` (API-key auth). A connection is a pair of channels; the `RealtimeConnector` trait lets tests script the server. |
| 0 | crate `audio` | `AudioIo`: default microphone and speakers, PCM16 mono 24 kHz, a playback queue that counts played samples and reports `Drained`, `clear_playback()` for barge-in. macOS: one `VoiceProcessingIO` unit (echo cancellation, format conversion). Elsewhere: `cpal` with linear resampling and half-duplex gating. |
| 3 | `code_assistant_core::voice` | `VoiceConfig`, `VoiceService`, `VoiceAgent` loop, `Floor` state machine, `NotificationQueue`, `NotificationSource`, voice tools, instructions. Audio comes in through the `VoiceAudio` trait / `AudioFactory`. |
| 4 | `ui_gpui` | Title-bar toggle, mute button, status chip, transcript popover, *Voice* settings section. |
| 5 | `code_assistant` | Feature `voice` (default). Adapts `audio::AudioIo` to `VoiceAudio` and runs the `VoiceService` worker next to the `SessionService` worker. |

The `audio` crate sits behind the `voice` feature, so headless builds
(`--no-default-features`) do not link CoreAudio, ALSA or WASAPI. The core
does not depend on it.

`cargo run -p audio --example devices [-- --silence | --tone]` opens the
devices for two seconds and reports captured chunks, playback and drains.

## Configuration

Stored in `<config_dir>/voice.json` and edited in a new *Voice* settings
section. It follows the pattern of `handoff.json` and `lifecycle.json`.

```json
{
  "provider": "openai",            // provider id from providers.json
  "model": "gpt-realtime",
  "voice": "marin",
  "url": "wss://…",                // optional, overrides the derived URL
  "transcription_model": "gpt-4o-mini-transcribe",  // "" = no transcript
  "vad_eagerness": "auto",         // semantic VAD: low | medium | high | auto
  "cooling_ms": 1500,
  "notify": "all"                  // "all" | "touched" (see Notifications)
}
```

Credentials come from the referenced `providers.json` entry, so nothing is
duplicated. Realtime models stay out of `models.json` on purpose: they cannot
run a normal session, and adding them there would put them into every model
picker. The settings section offers the providers that can serve realtime
sessions (`llm::realtime::connector_for_provider`):

- `openai`, `openai-responses`, `openai-responses-ws` with an `api_key`: the
  URL is `wss://<base_url host>/…/realtime?model=…` (or `url`).
- `ai-core`: the provider's client credentials and token manager; `model`
  names an entry of the provider's `models` map whose deployment serves the
  session. On every connect the connector reads the deployment resource
  (`…/v2/lm/deployments/{id}`), takes its `deploymentUrl`, appends
  `/v1/realtime` and connects with the bearer token and
  `AI-Resource-Group: default`. `session.update` then omits `model`.

The devices are the system defaults.

## Voice tools

All tools are thin adapters over `SessionService` / `session_query`
(`SessionService::session_content` reads a stored session without making
it the active one; `session_activity_states` gives the loaded sessions'
states). Every tool returns at once. None waits for a conversation's agent to finish.

| Tool | Parameters | Behaviour |
|---|---|---|
| `list_conversations` | `project?`, `include_settled?` (default false), `limit?` (default 20) | `list_sessions` + lifecycle + activity state → `[{id, title, project, branch, state, last_active}]`, where `state` is `running`, `idle`, `errored: …` or `waiting for …` (permission or answers). Sorted by recent activity. |
| `list_projects` | — | Project names, so `message_conversation` can create sessions in the right place. Can be merged into `list_conversations` later if it turns out to be noise. |
| `message_conversation` | `conversation_id?`, `project?`, `title?`, `message` | With no id, `create_session(title, project)` (the project must be known) and then send. With an id, `send_or_queue_user_message`. Returns `{conversation_id, status}` with `status` `started` or `queued`. Marks the session as *touched*. |
| `get_conversation` | `conversation_id`, `full?` (default false) | Default: `{state, last_agent_message}`, the final assistant text of the latest turn. With `full: true`: the last 10 turns, each with the user message and the turn's **last** assistant text only (no thinking, no tool calls, no tool results). Every text is truncated (about 2 000 characters each, with a total cap). Built on `session_query::get_session_content` with `UserText` + `AssistantText` plus grouping by turn. Calling it acknowledges any pending notification for that conversation. |

`message_conversation` replaces separate *create* and *comment* tools, as
suggested: an empty `conversation_id` means create. The voice agent never
reads the visible session or switches it. A later option is a
`show_conversation` tool that asks the GPUI frontend to open a session.

## Notifications

`NotificationSource` subscribes to the `EventStream` and watches
`UiEvent::UpdateSessionActivityState`:

- running → `Idle`: *finished*
- running → `Errored`: *failed*
- `RequestToolPermission` / `RequestUserQuestions`: *waiting for you*
  (v1 only reports it; it cannot answer)

With `notify: "all"` (the default), every session in this process is
reported. `"touched"` limits it to sessions the voice agent created or
messaged during this voice session. Sessions running in another process
(`RunningExternally`) send no events and are not covered.

On `StreamError::Lagged` the source gets back in sync by diffing
`list_sessions` activity against its last known states.

Each notification enters the `NotificationQueue`, keyed by session id. A
newer event for the same session replaces the older one, so a session that
finishes twice before it is flushed is reported once. A `get_conversation`
call for that session drops its entry: the model already knows.

When flushed, the queued entries are put into **one** text item. One
`response.create` follows:

> [background notification]
> - Conversation "Fix flaky test" in project proj-x (id …) finished its turn.
> - Conversation "Refactor parser" (id …) is waiting for permission to run
>   `execute_command`.
>
> The user is not speaking right now. If it is useful to them, mention this
> briefly. Call get_conversation before you summarise a result. Otherwise
> say nothing.

A "waiting for you" entry is withdrawn when the request is answered in the
UI before the floor frees up.

## Two-level priority: the `Floor` state machine

This is the core of the feature and the part to get right first. It is a
**pure** state machine. Inputs are events, outputs are commands; it has no
I/O and no clock (timers are commands the loop runs and reports back as
events). That makes it fully unit-testable.

Priority levels:

1. **Voice**: user speech, the model's answer to the user, and tool calls
   and results that belong to that answer.
2. **Background**: conversation notifications. They are only ever delivered
   when the floor is free, and never cancel or delay level 1.

The model generates audio much faster than it plays, so `response.done`
does **not** mean "stopped talking". Because we own the speakers natively,
the playback queue reports `Drained` exactly, with no browser round trip.
A fallback timer of `audio duration + 5 s` still guards against a stuck
sink.

### States

| State | Meaning |
|---|---|
| `Speaking` | a response is being generated **or** its audio is still playing |
| `UserTurn` | the user is speaking, or has spoken and is owed an answer |
| `Cooling` | playback drained; silence timer (`cooling_ms`) running |
| `Idle` | silent, nothing pending |

### Transitions

| Input | Effect |
|---|---|
| server `response.created` | cancel timers → `Speaking` |
| server `response.output_audio.delta` | (agent) queue for the speakers; remember the item and the sample offset its audio starts at |
| server `response.output_item.done` (function call) | (agent) spawn the tool → `ToolStarted` |
| server `response.done` / cancelled | attach held tool outputs; in `Speaking`, wait for the drain (playback timer: pending audio + 5 s), or act as drained if nothing is pending |
| server `input_audio_buffer.speech_started` (barge-in) | cancel timers; if `Speaking`: `response.cancel` (deferred until `response.created` when our create is still in flight) and `StopPlayback` (agent: clear the queue, `conversation.item.truncate` to the samples played). Audio deltas play only in `Speaking`, so late audio of the cancelled response is dropped → `UserTurn` |
| server `input_audio_buffer.speech_stopped` | stays `UserTurn`; the server's VAD creates the response. If none follows within 8 s the floor is free again |
| server `error` | when our `response.create` was in flight: give the floor back (no wedge) |
| sink `Drained` (or fallback timer) | if a trigger is pending (tool outputs attached during `Speaking`) → `response.create`; otherwise → `Cooling` + start the cooling timer |
| cooling timer fires | `Cooling` → `Idle`; flush notifications if any are queued |
| tool completed | response still open → hold until `response.done`; `Speaking` after generation → attach and set the pending trigger (fired on `Drained`); `UserTurn` → attach **without** `response.create` (the model uses it in its next answer); `Cooling`/`Idle` → attach + `response.create` |
| notification arrives | always queue; `Speaking`/`UserTurn` → nothing more; `Cooling` → restart the cooling timer (groups finishes that land close together); `Idle` → flush now |
| `get_conversation` read | drop that conversation's queued notification |

While a tool call runs, notifications are not flushed: the tool's result
makes the model speak anyway, and the notification follows after that
answer.

Invariants, each with its own test:

1. No `response.create` while a response is open.
2. No `response.create` in `UserTurn` caused by a tool result or a
   notification.
3. A notification is only flushed after `Drained` **and** `cooling_ms` of
   silence, or straight from `Idle`.
4. A notification never causes `response.cancel` or `flush()`.
5. Barge-in always truncates the server-side item to what was actually
   heard, so the model does not believe it said things the user never heard.

### Function calls inside a response

Voice tools run concurrently as spawned futures. Their outputs go through
the floor as *tool completed* events, never straight to the socket. So the
rules above apply equally to slow calls (`get_conversation` on a large
session) and to fast ones.

## Session lifetime and reconnect

- Start: open the realtime session → `session.update` (instructions, tools,
  voice, turn detection, input transcription on) → start capture.
- Realtime sessions have a maximum duration and can drop. The agent keeps a
  local text transcript (user transcripts + assistant transcripts + tool
  calls). On a closed connection it reconnects (three attempts, backing
  off), re-sends `session.update`, seeds the last 30 transcript lines as one
  system message, and keeps the notification queue (flushed after a cooling
  period). Tool results of the old connection are dropped.
- Stop: the toggle, or an error that cannot be retried. Capture and
  playback stop, the socket closes, and the queue is dropped.
- Mute: capture is paused, and notifications are still delivered (they only
  speak from `Idle`, which a muted user is in).
- Voice runs keep no transcript on disk in v1. The live transcript is kept
  in memory for the UI popover and for reconnect.

## UI (GPUI)

- Title bar: a microphone toggle (start/stop), and while active a status
  chip (Connecting… / Listening / Hearing you / Speaking / Muted, plus
  "· N waiting" for queued notifications) and a mute button. Events arrive
  as app-scoped `UiEvent::VoiceStatusChanged` and `UiEvent::VoiceTranscript`
  (`session_id: None` on the `EventStream`); `Gpui` mirrors them
  (`app/voice.rs`). A failure shows in the error popover.
- The status chip opens the transcript popover.
- Settings → *Voice*: provider, model, voice, transcription model, VAD
  eagerness, cooling time, notify scope.
- macOS bundle: `NSMicrophoneUsageDescription` in `Info.plist` and the
  `com.apple.security.device.audio-input` entitlement for the hardened
  runtime.
- The voice agent posts into sessions like a user would, so those sessions
  show the message as a normal user message. Later, a small "via voice"
  marker on the message could be added.

## Decisions

1. **Echo cancellation: native.** On macOS the `audio` crate drives a
   `VoiceProcessingIO` audio unit (the system's echo canceller, the one
   FaceTime uses) for capture and playback, so the user can interrupt the
   model while it speaks. Other platforms fall back to `cpal` with
   half-duplex gating: the microphone is dropped while audio plays, so there
   is no barge-in there.
2. **Provider: OpenAI Realtime protocol**, directly (`openai*` provider
   types) or through AI Core (`ai-core`, deployment from the provider's
   `models` map). `voice.json` can override the OpenAI URL for compatible
   gateways.
3. **Notify scope: all conversations** by default.

## Tests

- `voice::floor` — every transition and invariant, table-style.
- `voice::notifications`, `voice::source`, `voice::tools` (turn grouping
  and limits), `voice::config` (endpoint derivation).
- `voice::tests` — end to end: scripted realtime server, fake audio devices
  and a real `SessionService` with a mock LLM. A conversation finishing while
  the model speaks is announced only after drain plus cooling; a
  `message_conversation` call starts a session whose end is announced;
  `get_conversation` returns the turns and suppresses the notification;
  barge-in cancels and truncates to what was heard.
- `llm::realtime` — event parsing and serialization, PCM16 encoding.
- `audio` — playback queue accounting and drain reports, resampler.

## Follow-ups

- Answer permission prompts and questions by voice
  (`respond_permission`, `answer_questions` as voice tools, gated by an
  explicit confirmation step).
- `stop_conversation` (`request_stop`) and `show_conversation` (open it in
  the UI).
- Dictation into the normal composer, reusing the `audio` crate.
- Gemini Live transport.
- Choosing input and output devices.
