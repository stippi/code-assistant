# Terminal UI: Scroll-Region Architecture Plan (Backend-driven, Overlays, Dynamic Input)

This plan describes how to transform the current Terminal TUI to the scroll-region-based architecture validated in the terminal-ui prototype. The goals are:
- Backend-driven content: all content arrives as UiEvent/DisplayFragment from the backend task.
- Content-only native scrolling: use ANSI scroll region (DECSTBM) so the content area scrolls natively while a fixed input lives at the bottom.
- Dynamic input: multi-line input up to 5 lines with wrapping and virtual scroll; region updates as the input grows/shrinks and on resize.
- Streaming chunks: append at a virtual content cursor to visualize streaming without corrupting input lines.
- Overlays: transient UI (e.g., slash commands, session picker) rendered above input without interfering with content streaming.
- Single terminal I/O owner: all terminal writes serialized, no println! from multiple places.


## Current State Summary

Already present under crates/code_assistant/src/ui/terminal_tui:
- renderer.rs: Minimal TerminalRenderer that sets scroll region and can write messages / redraw a fixed-height input.
- app.rs: Event loop wiring backend, UI, and terminal. Uses TerminalRenderer for input and message writes. Basic prompt and key handling.
- ui.rs: TerminalTuiUI implements UserInterface; DisplayFragment/UiEvent paths forward to the renderer.write_message.

Gaps vs target:
- Input is single-line and doesn’t wrap/scroll or dynamically change height.
- No content-chunk appending API and column tracking for streaming.
- No overlay support.
- Width-sensitive wrapping (Unicode width) not applied.
- Some legacy ratatui-based paths and direct stdout use still exist outside this module.


## Architecture Overview

- Content region: rows 1..=content_bottom (top margin = 1).
- Input region: bottom input_height rows. input_height is dynamic (1..=5) based on buffer wrapping.
- Scroll region is always set to [1; rows - input_height]. Printing inside region wraps and scrolls only the content region; input rows are unaffected.
- All terminal writes go through TerminalRenderer behind a Mutex. No direct stdout elsewhere.
- The UI receives all content from backend events; the input loop never prints content directly.
- Overlays temporarily reduce the content region’s bottom (content_bottom -= overlay_height) and render their lines just above the input. When closed, restore the region.


## Components and Responsibilities

1) TerminalRenderer (extend crates/code_assistant/src/ui/terminal_tui/renderer.rs)
- Setup/teardown raw mode and scroll region.
- Maintain (cols, rows), input_height, and a virtual content cursor column (content_cursor_col).
- Methods:
  - new() -> Arc<Self>
  - write_message(&self, text: &str): simple write at bottom of region (existing)
  - append_content_chunk(&self, chunk: &str): move to bottom row + content_cursor_col, Print(chunk), update content_cursor_col using Unicode width, wrapping at cols; reset to 0 on '\n'.
  - redraw_input(&self, prompt: &str, input_lines: &[String], cursor_row: u16, cursor_col: u16)
    - Clears input area; renders up to input_height lines with a small prefix (e.g., "> ") on each line; positions cursor precisely.
  - set_input_height(&self, h: u16): recompute region bottom and apply DECSTBM; clamp 1..=5.
  - handle_resize(&self, new_cols, new_rows, ...): recompute dims; reapply region; clamp content_cursor_col; trigger an input redraw.
  - apply_overlay(&self, overlay_height: u16): temporarily shrink region bottom by overlay_height; clear and draw overlay lines above input; ensure subsequent content printing scrolls within the smaller region.
  - clear_overlay(&self): restore region bottom; clear overlay lines.

2) InputArea (new, modeled after the prototype)
- Buffer of text, cursor index in chars, max_lines = 5, terminal_width -> wrapped lines width = cols - prompt_width.
- Methods to insert/delete, move cursor, compute wrapped lines, display subset (with virtual scroll), and output display cursor (row, col).
- Public API returns:
  - display_height (<= 5)
  - display_lines: Vec<String>
  - display_cursor_pos: (row, col)
  - update_terminal_width(cols)
- This lives in the terminal_tui module (e.g., components/input_area.rs or directly in renderer.rs if we prefer minimal modules).

3) TerminalTuiUI (crates/code_assistant/src/ui/terminal_tui/ui.rs)
- Continues to implement UserInterface.
- For UiEvent and fragments, call renderer.append_content_chunk(...) (not println!).
- No direct stdout; all printing via renderer.
- Maintains shared AppState (messages, sessions, activity state) as today; display_fragment remains a pass-through to renderer with minimal formatting.

4) TerminalTuiApp (crates/code_assistant/src/ui/terminal_tui/app.rs)
- Owns the input handling loop using crossterm events.
- Maintains InputArea for editing with multi-line support and cursor movement (Enter submits; Shift+Enter inserts newline).
- On every edit or on tick, recompute input display (wrapped), call renderer.set_input_height(display_height) and renderer.redraw_input(...).
- On Enter, send BackendEvent::{SendUserMessage|QueueUserMessage}, clear InputArea.
- On Event::Resize, update InputArea width and call renderer.handle_resize().
- Exposes overlay toggles (e.g., slash commands) that:
  - set overlay state in AppState
  - call renderer.apply_overlay(height) and print overlay contents
  - while overlay is open, buffer incoming content prints or allow content to scroll in the smaller region; choose per overlay type.


## Overlays

- Strategy: render overlays in the bottom of the content region, just above the input.
- While an overlay is active with height H, we set the scroll region to [1; rows - input_height - H].
- Render overlay lines in rows (rows - input_height - H) .. (rows - input_height - 1).
- On close, clear those rows and restore region bottom to rows - input_height.
- Incoming content continues to scroll inside the smaller region; if the overlay should freeze content, we buffer DisplayFragment writes in UI state and flush on close.
- Examples: a session list (few lines), command help, quick picker.


## Concurrency and Serialization

- Single writer: TerminalRenderer guards stdout. All writes funnel through it. The backend emits UiEvent/DisplayFragment, and TerminalTuiUI forwards them to the renderer.
- The input loop never writes content directly; it only calls redraw_input and region updates.
- Avoid interleaving sequences: serialize append_content_chunk and redraw_input; when printing a chunk, immediately follow by redraw_input so the cursor ends anchored in input.


## Scrollback

- Keep top margin at 1 (ESC[1;bottom r]) so lines that scroll off the top contribute to terminal scrollback in most terminals.
- Do not use the alternate screen buffer.


## Unicode Width

- Use unicode-width to account for wide characters when updating content_cursor_col.
- For input wrapping, compute display width with unicode-width as well.


## Migration Plan (Phased)

Phase 1: Strengthen TerminalRenderer + InputArea
- Add InputArea component (copy/port from prototype) with max_lines=5 and wrapping.
- Extend TerminalRenderer:
  - set_input_height, append_content_chunk, apply_overlay/clear_overlay, unicode width handling.
  - redraw_input now takes multi-line input (lines + cursor pos) and renders per line with "> ".
- Wire unicode-width in Cargo.toml for code_assistant crate if not present.

Validation:
- cargo check; basic manual test in TerminalTuiApp: typing grows input up to 5 lines; background DisplayFragment prints stream without affecting input.

Phase 2: Integrate InputArea into TerminalTuiApp
- Replace single-line input_buffer logic with InputArea.
- On keystrokes: update InputArea; compute display_height; call renderer.set_input_height(display_height) then renderer.redraw_input.
- Enter submits to backend and clears InputArea.
- Resize: update width in InputArea and renderer.handle_resize; redraw.

Validation:
- Manual: type long lines wrapping over multiple rows; Shift+Enter line breaks; stream chunks concurrently.

Phase 3: UI printing via append_content_chunk
- Update TerminalTuiUI::display_fragment and UiEvent handlers that write to call renderer.append_content_chunk instead of renderer.write_message.
- Ensure a trailing "\n" where message boundaries require new lines.
- Immediately after each append, call renderer.redraw_input with the current input view (fetch from a shared input state or have TerminalTuiApp provide a small handle that returns the latest input view).

Validation:
- Streaming fragments arrive as chunks and visibly flow; input cursor remains stable.

Phase 4: Overlays
- Introduce an OverlayState (None | SessionPicker { items, selected } | Help | ... ).
- Implement renderer.apply_overlay/clear_overlay.
- Draw a small overlay (e.g., session picker) and ensure content continues to scroll under it or is paused depending on choice.
- Close overlay on Esc or selection; restore region; redraw input.

Validation:
- Instruct user to open overlay while streaming; verify no corruption; close overlay; content and input restore correctly.

Phase 5: Cleanup remaining stdout and legacy paths
- Search for direct stdout writes in terminal paths and convert to renderer calls.
- Ensure old ratatui terminal path is either feature-gated or deprecated; keep GPUI unaffected.

Validation:
- cargo clippy -- -D warnings
- cargo test


## Interface Changes and References Checklist

- If adding InputArea module, update imports in terminal_tui::app.rs to use it.
- Replace any println!/writeln! in terminal_tui/* with renderer.append_content_chunk or renderer.write_message.
- Ensure TerminalTuiUI owns a reference to TerminalRenderer (already present) and never writes directly to stdout.
- On adding unicode-width, update Cargo.toml accordingly.
- If you add overlay APIs in TerminalRenderer, ensure they’re called only from the app (not the backend UI path).


## Acceptance Criteria

- Content streaming:
  - Chunks appear immediately, wrap correctly, and scroll only the content region.
  - Input remains interactive and fixed at the bottom, growing up to 5 lines.
  - Resizing preserves correctness and reflows input.
- Overlays:
  - A simple overlay (session picker) can appear above input without content corruption; when active, content keeps scrolling in a reduced region or is buffered.
- Concurrency:
  - All terminal writes go through a single renderer; no interleaving artifacts.
- Scrollback:
  - History remains in terminal scrollback; alternate screen is not used.
- Code quality:
  - No direct stdout usage in the new terminal path; unicode width is handled; clippy clean.


## Notes

- Terminals vary in DECSTBM behavior for scrollback; by keeping top margin at 1, most xterm-like emulators record scrolled-off lines.
- If a terminal lacks proper DECSTBM support, a fallback can set no region and re-render input after prints (reduced UX but functional).
