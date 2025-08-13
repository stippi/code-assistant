# Terminal UI Parity Plan

This plan analyzes the current GPUI and Terminal implementations, identifies reusable parts, proposes a new terminal architecture that reuses the GPUI "backend" and session manager, and defines phased, verifiable steps to implement it. Each phase is designed to compile and pass tests/lints incrementally.


## High-level goals

- Reuse the existing multi-session backend (currently under `ui/gpui/backend.rs`) and `SessionManager` for Terminal UI.
- Unify UI-to-backend communication into a shared module so both GPUI and Terminal consume the same event types and backend handler.
- Build a robust terminal UI with:
  - Always-available input area at the bottom (cursor can move; edits while agent streams)
  - Streaming output with reflow on resize
  - Overlays for session switching/management
  - Pending messages and queuing while an agent is running
  - Stable rendering and performance
- Keep tests passing throughout, and gate larger changes with feature flags if needed.


## Summary of current code

- Shared abstractions:
  - `ui::UserInterface` trait with `send_event(UiEvent)` and `display_fragment(DisplayFragment)`.
  - `ui::ui_events::{UiEvent, MessageData, ToolResultData}` used by the agent/session layer to inform the UI.
  - `ui::streaming` processors to convert model output into fragments, used by session instances.
  - `session::{SessionManager, instance::SessionInstance, instance::ProxyUI}` implement multi-session logic, fragment buffering, activity state, and pending message handling.

- GPUI specifics:
  - `ui/gpui/mod.rs` defines GPUI components and UI state management.
  - `ui/gpui/backend.rs` contains the backend bridge that:
    - Receives BackendEvent
    - Mutates `SessionManager`
    - Emits UI updates via `UiEvent` on the `UserInterface` (Gpui) instance
  - `app/gpui.rs`:
    - Creates `Gpui`
    - Sets up unified channels `(BackendEvent, BackendResponse)`
    - Spawns a backend task running `handle_backend_events`
    - Runs GPUI's own event loop and windows.

- Terminal specifics (current):
  - `ui/terminal.rs` implements `UserInterface` but prints directly to stdout and manages a spinner.
  - `app/terminal.rs` is a blocking loop using rustyline for input; after each user input, it triggers a single agent iteration; limited concurrency and no persistent input while streaming.
  - Terminal does not use the backend bridge or unified channels; session management commands are ad-hoc via `:` commands.


## Key reuse and refactoring opportunities

- Move GPUI backend event types and handler out of `ui/gpui` into a shared `ui/backend` module:
  - Types: `BackendEvent`, `BackendResponse`
  - Function: `handle_backend_events`
  - Generalize to accept any `Arc<dyn UserInterface>` implementation for UI event emission (already used that way via `Gpui` clone; just move and rename imports)
- Terminal should adopt the same backend bridge and unified channels as GPUI:
  - Terminal app creates channels via a `setup_backend_communication()`-like function (Terminal-side equivalent) or reuse a small helper.
  - Spawn `handle_backend_events` with the same `SessionManager`.
  - Terminal UI implements `UserInterface` and consumes `UiEvent` updates to maintain state and redraw.
- Keep `SessionManager`, `SessionInstance`, `ProxyUI`, and `UiEvent` unchanged and reused by both UIs.


## Proposed terminal architecture (new)

- Libraries:
  - ratatui (https://github.com/ratatui-org/ratatui): terminal UI framework with layout, widgets, popups, and resize support.
  - crossterm: already used; ratatui works well on top of it.
  - tui-textarea (https://github.com/rhysd/tui-textarea): multiline text input widget with cursor movement, selection, and editing; suitable for the always-available input at the bottom.

- Visual style: minimalist and modern. Avoid emojis altogether. Use subtle colors, whitespace, and clean typography-like spacing. Status indicators (spinner/rate-limit) appear inline near the assistant output start, not as overlays.

- Structure under `ui/terminal_tui/` (new folder):
  - `mod.rs`: module root exposing Terminal TUI types
  - `app.rs`: orchestrates event loop
    - Sets up terminal backend (enter/leave raw mode)
    - Owns channels to backend (`BackendEvent`, `BackendResponse`)
    - Spawns tasks:
      - Backend handler: `handle_backend_events`
      - UI event receiver: consumes `UiEvent` and updates state
      - Input handler: reads `crossterm` events and updates the `InputArea` state
      - Render loop: redraws on a tick or on state changes (using a watch channel)
  - `state.rs`: shared app state (messages, tools, activity state, pending message, sessions list, current session id, working memory)
  - `components/`:
    - `messages.rs`: scrollable message view, renders `MessageData` and ongoing streaming fragments
    - `input.rs`: wrapper around `tui_textarea::TextArea` plus attachments (phase 2)
    - `sidebar.rs`: sessions list, item selection, activity indicators
    - Inline status in messages: spinner character and short text near the start of the upcoming assistant output; no overlays for spinner or rate limit
  - `ui.rs`: `TerminalTuiUI` implementing `UserInterface`
    - Receives `UiEvent` and `DisplayFragment`
    - Updates `state` and signals redraw
    - Honors `should_streaming_continue` (e.g., esc/cancel) and rate limit notifications

- UX mapping to GPUI:
  - Pending message queuing: If `SessionActivityState != Idle`, hitting Enter queues the message via `UiEvent::QueueUserMessage` instead of sending; show pending line in input or a small badge; backspace edits the pending message.
  - Session switching UI: `sidebar.rs` can slide in/out or appear as a minimalist modal list when invoked (e.g., via a keybinding like `Ctrl+S`). Keep appearance clean and minimal; no decorative elements or emojis.
  - Keybindings:
    - Enter: submit (send or queue, depending on activity)
    - Shift+Enter: new line (mirroring GPUI behavior)
    - Esc: cancel/stop current agent (mirrors GPUI Cancel)
    - Ctrl+K: focus input; Ctrl+S: toggle sessions; others as needed


## Phased implementation plan

Each phase ends with checks:
- cargo check
- cargo test
- cargo clippy -- -D warnings

Where code touches public APIs, we search for all usages and update imports accordingly.


### Phase 0 — Baseline and guardrails

- Ensure repository builds and tests pass.
- Optionally add CI clippy check (if not present) to surface new warnings early.

Validation:
- cargo check
- cargo test
- cargo clippy -- -D warnings


### Phase 1 — Extract shared backend module

Goal: Move GPUI backend bridge to a shared `ui/backend.rs` module without behavior changes.

Tasks:
- Create `crates/code_assistant/src/ui/backend.rs` with:
  - `pub enum BackendEvent` (moved from `ui/gpui/mod.rs`)
  - `pub enum BackendResponse` (moved from `ui/gpui/mod.rs`)
  - `pub async fn handle_backend_events(...)` (moved from `ui/gpui/backend.rs`)
- Update code:
  - In GPUI code, replace `crate::ui::gpui::BackendEvent` -> `crate::ui::backend::BackendEvent`
  - Replace `crate::ui::gpui::BackendResponse` -> `crate::ui::backend::BackendResponse`
  - Move and update imports in the moved `backend.rs` to not depend on `gpui::*` except for `UserInterface` and `UiEvent`.
  - Ensure `handle_backend_events` takes and uses a generic `Arc<dyn UserInterface>` where needed, not `Gpui`-specific types (it already uses `UserInterface` for UI event emission; only the type path changes).
- Search and update all references across the repo.

Validation:
- cargo check
- cargo test
- cargo clippy -- -D warnings


### Phase 2 — Terminal TUI skeleton (feature-gated)

Goal: Introduce a new terminal TUI module with minimal compile-time integration, behind a feature flag to keep the current terminal behavior and tests intact.

Tasks:
- Add dependencies in `crates/code_assistant/Cargo.toml` (optional features):
  - Use `cargo add ratatui --features crossterm --no-default-features`
  - Use `cargo add tui-textarea`
  - Ensure `crossterm` remains
- Create `ui/terminal_tui/` with minimal files:
  - `mod.rs`: pub use TerminalTuiApp
  - `app.rs`: defines `TerminalTuiApp::new/run()` stubs that return Ok(()), no rendering yet
  - `state.rs`: defines empty state struct
  - `ui.rs`: defines `TerminalTuiUI` implementing `UserInterface` with no-op methods
- In `app/terminal.rs`, introduce a `--experimental-tui` code path (via env or cfg feature):
  - If feature enabled, call `TerminalTuiApp::run(config)`; else keep existing implementation.
- Do not remove `ui/terminal.rs` nor its tests; they continue to pass.

Validation:
- cargo check
- cargo test
- cargo clippy -- -D warnings


### Phase 3 — Wire shared backend into Terminal TUI

Goal: Terminal TUI uses the shared `ui::backend::{BackendEvent, BackendResponse, handle_backend_events}` and `SessionManager` just like GPUI.

Tasks:
- In `ui/terminal_tui/app.rs`:
  - Create channels `(async_channel::Sender<BackendEvent>, Receiver<BackendResponse>)` similar to `Gpui::setup_backend_communication()` (create a small helper for terminal).
  - Build `SessionManager` (same as in `app/gpui.rs`), wrap in `Arc<tokio::sync::Mutex<_>>`.
  - Spawn a tokio task that runs `handle_backend_events(event_rx, response_tx, multi_session_manager, Arc<LLMClientConfig>, terminal_ui_clone)`
  - Initialize LLMClientConfig from `AgentRunConfig` (record/playback supported).
- Implement `TerminalTuiUI` to:
  - Maintain a shared `AppState` (Arc<Mutex<_>>) with message list, tool statuses, working memory, session list, current session id, activity state, pending message.
  - On `send_event`, update `AppState` (mirror logic in GPUI’s `process_ui_event_async` but for terminal state) and notify a redraw channel.
  - On `display_fragment`, update the last assistant message buffer in state and notify a redraw.
  - Honor `should_streaming_continue` based on a cancel flag in `AppState`.
  - Handle `notify_rate_limit/clear_rate_limit` with state flags.
- In `TerminalTuiApp::run()`, set initial session (create or connect to latest) by sending `BackendEvent` like GPUI’s `run()` does.

Validation:
- cargo check
- cargo test (existing tests continue to use old `TerminalUI`)
- cargo clippy -- -D warnings


### Phase 4 — Rendering loop with ratatui

Goal: Implement actual drawing with ratatui using `AppState`.

Tasks:
- Initialize terminal backend and raw mode in `TerminalTuiApp::run()`; ensure proper cleanup with a guard.
- Create a render loop:
  - Consume a `tokio::sync::watch::Receiver<()>` or `tokio::sync::Notify` to trigger redraws
  - Also tick on a small interval (e.g., 16–33ms) for spinners and smooth updates
  - Draw layout: vertical split
    - Top: scrollable `messages` panel (streaming output, tool usage blocks; simple first)
    - Bottom: input area (`tui_textarea`)
  - Reflow text by constraining width to current terminal size; ratatui handles wrapping.
- Input handling task:
  - Use `crossterm::event::read()` in a tokio-friendly way (spawn blocking) or `crossterm::event::EventStream`
  - Map keys: Enter (send/queue), Shift+Enter (newline), Esc (cancel)
  - Update input state and send `BackendEvent` via channel on submit: either `SendUserMessage` or `QueueUserMessage` depending on `AppState.activity_state`
- Session list UI:
  - Toggle with a key (e.g., Ctrl+S)
  - Render a minimalist list with selection and invoke `BackendEvent::LoadSession/DeleteSession/CreateSession`

Validation:
- cargo check
- cargo clippy -- -D warnings
- Manual run: resize terminal; confirm reflow; type while streaming


### Phase 5 — Tool rendering and attachments

Goal: Achieve feature parity for tool streaming and attachments.

Tasks:
- Messages view renders:
  - Thinking blocks (styled dim/italic)
  - Tool sections with name, streaming parameters (name printed once; value streaming appended), and end markers
  - Tool result updates from `UiEvent::UpdateToolStatus/EndTool`
- Input attachments (Phase 5.1 or later):
  - For terminal, render attachment chips above input; initially support only text and file names; images as placeholders
  - Paste handling can be deferred; accept a future `:attach` command to add a file path

Validation:
- cargo check
- cargo clippy -- -D warnings


### Phase 6 — Replace old Terminal path (optional)

Goal: Make the new TUI the default terminal experience while preserving tests that exercise formatting logic.

Tasks:
- Switch `app/terminal.rs` to call `TerminalTuiApp::run(config)` by default; keep `--legacy-terminal` flag to use `ui/terminal.rs` runner for existing tests.
- Option 1: Keep `ui/terminal.rs` and its tests unchanged (they validate `DisplayFragment` formatting logic, still useful)
- Option 2: Port tests to target `TerminalTuiUI` state updates; this can come later.

Validation:
- cargo check
- cargo test
- cargo clippy -- -D warnings


### Phase 7 — Polish and parity checks

- Visual polish: colors, inline status indicators (rate limited / waiting for response), scrollback behavior
- Draft persistence in terminal (mirror GPUI `DraftStorage`): load/save drafts per session
- Robust cancellation and stop requests (mirror GPUI logic using `session_stop_requests` equivalent in `AppState`)
- Performance profiling on large histories and rapid streaming

Validation:
- cargo check
- cargo test
- cargo clippy -- -D warnings
- Manual run parity checklist


## Interface changes and references checklist

Whenever moving `BackendEvent/BackendResponse` and `handle_backend_events`, search and update:
- Imports in `ui/gpui/*`
- Imports in `app/gpui.rs`
- Any references in tests or session management code

Commands:
- ripgrep (or search tool) for `BackendEvent` and `BackendResponse` and `handle_backend_events`


## Risks and mitigations

- Risk: Breaking GPUI by moving backend types
  - Mitigation: Do the move in Phase 1 only; keep behavior identical; run GUI locally to sanity check
- Risk: Terminal tests coupling to `TerminalUI`
  - Mitigation: Keep legacy `TerminalUI` and tests; introduce new TUI behind a feature flag until stable
- Risk: Input handling in a TUI is complex
  - Mitigation: Use `tui-textarea` for editing; start with minimal bindings; incrementally add features


## Acceptance criteria

- Terminal can:
  - Show session history and ongoing agent output while typing
  - Queue messages while agent is running
  - Switch sessions via overlay without losing stream state
  - Resize cleanly and reflow content
  - Show rate-limited and waiting states
- Both UIs use the same backend bridge and session manager
- Clean clippy and passing tests


## Appendix: Suggested crates

- ratatui: mature, actively maintained fork of tui-rs
- crossterm: cross-platform terminal handling
- tui-textarea: text input widget for ratatui

These are widely used in the Rust ecosystem for TUIs.
