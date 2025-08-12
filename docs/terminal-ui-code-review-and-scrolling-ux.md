# Terminal UI: Code Review + Scrolling UX Plan

This document summarizes the code review feedback and lays out a concrete plan to deliver the scrolling behavior you described: native terminal scrollback for message history, with an input that stays fixed at the bottom and is always ready, while the assistant streams.


## Why not ratatui viewport + tui-textarea for this UX?

- Native scrollback and gestures: ratatui’s viewport keeps content inside the TUI, so users don’t get the terminal’s native scrollback (mouse wheel/touch gestures). You end up implementing a custom scrollbar and scroll logic, which feels foreign and often awkward.
- Persistent output after exit: You want the conversation to remain visible after exiting. Full-screen TUIs typically draw on the alternate screen and restore on exit (clearing the history). Even if you avoid the alternate screen, drawing the messages via ratatui still keeps them “owned” by the app; you must dump them yourself to the main buffer to preserve them.
- Performance and robustness: Redrawing entire frames on every tick or event is heavier and can produce artifacts as output grows. Printing messages directly to stdout uses the terminal’s native buffering and wrapping, avoids unnecessary redraws, and scales better for long sessions.
- Input pinned at bottom: ratatui can simulate this, but you’re still fighting the redraw loop and viewports. Using an ANSI scroll region to reserve a few lines for input is a simpler, more robust primitive that terminals implement efficiently.
- UX fidelity: Users of CLI tools expect native selection, copy, and scrollback to work as they do in the terminal. A minimal, direct-stdout approach preserves those expectations.

In short, implementing our own “viewport” using ANSI scroll regions gives you the exact behavior you want with less complexity and tighter alignment with terminal expectations.


## Code review feedback summary

1) Unify backend bridge
- Remove ui/gpui/backend.rs; keep a single shared ui/backend.rs with BackendEvent, BackendResponse, handle_backend_events.
- Update app/gpui.rs to use ui::backend::handle_backend_events (gpui/mod.rs already re-exports BackendEvent/Response).

2) Stop mixing stdout prints with TUI draws
- TerminalTuiUI::send_event/display_fragment currently prints to stdout; this conflicts with ratatui drawing and causes flicker/overlap.
- Choose one model. For the desired UX, prefer direct stdout printing for messages and reserve the bottom for the input bar using ANSI scroll regions.

3) Share one AppState
- TerminalTuiApp holds an AppState and TerminalTuiUI creates its own. This splits state; the renderer and UI disagree.
- Pass Arc<Mutex<AppState>> into TerminalTuiUI::new(shared_state) so both layers operate on the same state.

4) Handle backend responses in TUI
- Spawn a task that reads BackendResponse and translates them into UiEvent updates (e.g., SessionsListed -> UiEvent::UpdateChatList). Also request BackendEvent::ListSessions after create/load.

5) Inline status indicators; no overlays, no emojis
- Replace emojis with minimal labels/colors. Spinner/rate-limit should be inline (first line of assistant output or a subtle marker in the prompt), not overlays.

6) Cancel behavior
- Reset cancel flag on StreamingStopped and StreamingStarted (or on next send). Avoid sticky cancellation.

7) Remove raw-mode hacks for scrolling
- Don’t toggle raw mode off to instruct users to use native scroll. With the scroll region approach, native scroll works without mode flipping.

8) Keep layout minimal and modern
- Minimal borders and titles for the bottom input. No emojis, subtle color only.

9) Dependency versions
- If desired, switch to using cargo add for new crates to follow current versions automatically.


## Scrolling UX: detailed plan using ANSI scroll regions

Goal: Always-visible input at the bottom; messages printed to main buffer; native terminal scrollback; resize and reflow behave naturally.

Key technique: Set a terminal scrolling region (DECSTBM) so the top region scrolls while the bottom N lines remain fixed for input.

- On startup:
  - Enable raw mode (crossterm::terminal::enable_raw_mode()).
  - Query terminal size (cols, rows).
  - Choose input_height (2–3 lines).
  - Set scroll region to [1 .. rows - input_height]. Escape sequence: ESC[{top};{bottom}r with top=1, bottom=rows - input_height.
  - Draw the initial empty input bar in the bottom region and place the cursor at the input cursor position.

- On exit:
  - Reset scroll region to full screen (ESC[r]).
  - Disable raw mode and show cursor.

- Printing messages (UiEvent/display_fragment):
  - Acquire a stdout lock (to serialize prints with input redraws).
  - Move cursor to the last line of the scroll region; write the content; newline will scroll the scroll region upward, contributing to the native scrollback.
  - After any message output, call redraw_input() to re-render the bottom input bar in-place.

- Input handling:
  - A small input component that keeps text buffer and cursor position.
  - Handle keys: Enter (submit), Shift+Enter (newline), Esc (cancel), Left/Right/Home/End, etc.
  - On any change, call redraw_input().
  - Enter triggers either SendUserMessage or QueueUserMessage depending on current SessionActivityState.

- Redrawing the input (redraw_input):
  - Compute available width from current terminal size.
  - Move cursor to the top-left of the input region (row = rows - input_height + 1).
  - Clear the input area lines.
  - Render prompt + input text wrapped to width.
  - Reposition the cursor to the correct (x, y) within the input.
  - Keep styling minimal (subtle color on the prompt).

- Resizing:
  - On crossterm::event::Event::Resize(cols, rows):
    - Recompute bottom region start = rows - input_height + 1.
    - Reset the scroll region to the new size (ESC[r] then ESC[{top};{bottom}r).
    - Redraw the input at the new width.

- Rate limit and waiting indicators:
  - Inline only. Options:
    - At the start of the next assistant output line, include a small spinner and/or text like “waiting…” / “retrying in Ns…”. Once real content arrives, stop printing the spinner.
    - Alternatively, show a very subtle status glyph in the prompt (non-animated or very slow to avoid continual redraws).

- Session switching:
  - Keep minimal colon-commands (:sessions, :switch, :new, :delete) to avoid complex in-place overlays.
  - If a transient UI is needed, print a small selectable list above the input (in the scroll region) and then clear it. No alternate screen.

- Cancellation:
  - Maintain a cancel flag; clear it on StreamingStarted/Stopped or on next send.

- Concurrency:
  - Serialize stdout operations (message prints, input redraws) behind a Mutex to avoid interleaving.
  - UiEvent/display_fragment handlers print messages and then request an input redraw.

- Portability:
  - Scroll region (ESC[{top};{bottom}r) is widely supported (xterm, iTerm2, most Linux terminals; Windows 10+ with VT enabled via crossterm). Add a feature toggle or runtime detection to gracefully degrade if needed.


## Implementation steps from current code

1) Unify backend
- Delete ui/gpui/backend.rs. Update app/gpui.rs to import ui::backend::handle_backend_events.

2) Replace ratatui frame loop with a TerminalRenderer
- Remove the ratatui draw loop for messages; keep only the input bar redraw logic using crossterm and ANSI sequences.
- Introduce a TerminalRenderer with methods:
  - setup_scroll_region(input_height)
  - reset_scroll_region()
  - write_message_line(str)
  - redraw_input(prompt, input_buffer, cursor_pos)
  - handle_resize(cols, rows)

3) Share AppState
- Create Arc<Mutex<AppState>> once in TerminalTuiApp and pass it into TerminalTuiUI::new(shared_state).
- All UiEvent/display_fragment handlers update this shared state and then call renderer.redraw_input().

4) Backend response task
- Spawn a task that consumes BackendResponse and emits UiEvents (UpdateChatList, PendingMessageUpdated, etc.) to TerminalTuiUI.send_event().
- After CreateNewSession/LoadSession, send BackendEvent::ListSessions.

5) Input component
- Implement a BottomInput using crossterm events, with buffer and cursor management and multi-line support (Shift+Enter).
- On Enter, send SendUserMessage or QueueUserMessage based on SessionActivityState.
- On any change, call redraw_input().

6) Minimal inline status
- When WaitingForResponse/RateLimited, prefix the next assistant line with a short status marker.
- Optionally, place a small non-animated marker in the prompt to avoid constant redraws.

7) Clean styling
- Remove emojis across terminal paths. Use minimal color. Keep the input bar subtle.

8) Testing
- Manual tests:
  - Type while assistant streams; input remains usable and visible.
  - Resize terminal; input adjusts; messages continue to print naturally.
  - Scroll up with mouse/touch; input scrolls out of view naturally; scroll back down shows the input.
  - Exit; transcript remains in terminal scrollback.


## Escape sequences and crossterm pointers

- Set scroll region: ESC[{top};{bottom}r (e.g., “\x1b[1;{bottom}r”). Reset: ESC[r].
- Move cursor: crossterm::cursor::MoveTo(x, y) (0-based); combine with Print and Clear(ClearType::CurrentLine) to render input.
- Raw mode: crossterm::terminal::{enable_raw_mode, disable_raw_mode}.
- Resize events: crossterm::event::Event::Resize.


## Notes on fallback behavior

If scroll region support isn’t present, the UI can:
- Skip setting the region; simply redraw the input after each output. The input may scroll occasionally, but the app remains usable and native scrollback still works.


## Conclusion

This approach keeps the terminal experience native, robust, and minimal: messages stream in the main buffer for perfect scrollback, and a small pinned input is redrawn in-place. It avoids complex frame redraws and custom scrollbars, aligns with your preferences, and reuses the shared backend and session architecture already in place.
