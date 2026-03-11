# Terminal Cards Implementation Plan

**Branch**: `feature/terminal-cards`
**Goal**: Render execute_command tool output as embedded terminal cards in the GPUI UI, with full ANSI color support, similar to Zed's Agent Panel.

## Architecture

Two new workspace crates, consumed by `code_assistant`:

```
crates/
  terminal/          # New: alacritty_terminal wrapper, GPUI entity
  terminal_view/     # New: GPUI Element that renders the terminal grid
  code_assistant/    # Existing: consumes both new crates
```

### Key Dependencies

| Crate | Depends On |
|-------|-----------|
| `terminal` | `alacritty_terminal` (Apache-2.0), `gpui` (for Entity/EventEmitter) |
| `terminal_view` | `terminal`, `gpui`, `gpui-component` |
| `code_assistant` | `terminal`, `terminal_view` (+ existing deps) |

### Reference Code
Zed's implementation in the `zed` project (checked out locally):
- `crates/terminal/src/terminal.rs` — Terminal entity, PTY wiring, content snapshots
- `crates/terminal_view/src/terminal_element.rs` — GPUI Element for grid rendering
- `crates/agent_ui/src/connection_view/thread_view.rs:4687-4970` — Card UI wrapper
- `crates/agent_ui/src/entry_view_state.rs:377-396` — Embedded mode setup

---

## Phases

### Phase 1: `terminal` crate — Alacritty integration
> Minimal terminal entity that can run a command via PTY and provide content snapshots.

- [x] **1.1** Create crate skeleton: `Cargo.toml`, `src/lib.rs`
- [x] **1.2** Add `alacritty_terminal` dependency (pinned to Zed's fork for compat, or crates.io version)
- [x] **1.3** Implement `ZedListener` — bridges `alacritty_terminal::event::EventListener` to an unbounded channel
- [x] **1.4** Implement `TerminalBounds` — implements `alacritty_terminal::grid::Dimensions` for cell sizing
- [x] **1.5** Implement `TerminalContent` — snapshot struct: cells, cursor, mode, display offset
- [x] **1.6** Implement `Terminal` struct — GPUI entity holding `Arc<FairMutex<Term>>`, event queue, last_content
- [x] **1.7** Implement `TerminalBuilder` — factory that creates PTY, Term, EventLoop, wires everything up
- [x] **1.8** Implement `Terminal::sync()` — lock term, drain events, produce `TerminalContent` snapshot
- [x] **1.9** Implement `Terminal::total_lines()`, `Terminal::set_size()`
- [x] **1.10** Implement event processing — translate `AlacTermEvent` to our own `Event` enum, emit via GPUI
- [x] **1.11** Implement child exit detection — detect when command finishes, capture exit status
- [x] **1.12** Basic test: create terminal, run `echo hello`, verify content contains "hello"

### Phase 2: `terminal_view` crate — GPUI rendering
> A GPUI Element that paints the terminal grid with colors.

- [x] **2.1** Create crate skeleton: `Cargo.toml`, `src/lib.rs`
- [x] **2.2** Implement `TerminalElement` struct — holds `Entity<Terminal>`, font config
- [x] **2.3** Implement `layout_grid()` — iterate cells, produce `LayoutRect` (backgrounds) and `BatchedTextRun` (styled text)
- [x] **2.4** Implement `cell_style()` — convert alacritty cell flags/colors to GPUI `TextRun`
- [x] **2.5** Implement `convert_color()` — map ANSI colors (named, indexed, true color) to GPUI `Hsla`, using theme's `terminal_ansi_*` colors where available
- [x] **2.6** Implement `Element` trait for `TerminalElement`:
  - `request_layout()` — compute size based on content mode (inline vs scrollable)
  - `prepaint()` — compute font metrics, call `sync()`, run `layout_grid()`
  - `paint()` — fill background, paint `LayoutRect`s, paint `BatchedTextRun`s via `shape_line`
- [x] **2.7** Implement `TerminalView` entity — wraps `TerminalElement`, manages embedded mode (inline growing vs fixed height)
- [x] **2.8** Implement `ContentMode` enum — `Inline { displayed_lines, total_lines }` vs `Scrollable`
- [x] **2.9** Visual test: render a terminal running `ls --color=auto` in a test window

### Phase 3: Integration into code_assistant
> Wire up the terminal crates to the execute_command tool and GPUI UI.

- [x] **3.1** Add `terminal` and `terminal_view` as dependencies of `code_assistant`
- [x] **3.2** Implement `ExecuteCommandOutputRenderer` — a `ToolOutputRenderer` that creates and embeds a `TerminalView`
  - Alternative: create a new `BlockData` variant for terminal blocks (evaluate which approach is cleaner)
- [ ] **3.3** Modify `execute_command` tool to create a `Terminal` entity for each command execution
- [ ] **3.4** Wire streaming: instead of (or in addition to) `StreamingCallback::on_output_chunk()`, feed output to the `Terminal` entity
- [ ] **3.5** Implement terminal card wrapper UI:
  - Header: working directory, running indicator, elapsed time
  - Command display: monospace, collapsible
  - Body: embedded `TerminalView`
  - Error state: dashed border on failure
- [ ] **3.6** Handle expand/collapse toggle for terminal cards
- [ ] **3.7** Handle command completion: show exit status, stop indicator animation

### Phase 4: Polish & edge cases
> Make it production-ready.

- [ ] **4.1** Theme integration: map ANSI colors to the active gpui-component theme
- [ ] **4.2** Handle long output: switch to scrollable mode when output exceeds threshold
- [ ] **4.3** Handle terminal resize when the panel resizes
- [ ] **4.4** Truncation indicator for very long outputs
- [ ] **4.5** Copy button for command text
- [ ] **4.6** Ensure terminal UI works in both light and dark themes
- [ ] **4.7** Performance: verify smooth rendering with large outputs (1000+ lines)
- [ ] **4.8** Cleanup: ensure terminals are properly dropped when sessions end

---

## Design Decisions

### PTY vs Display-Only
We'll start with **display-only mode** (no PTY), feeding command output text directly via `write_output()`. This is simpler because:
- The `command_executor` already captures output as text chunks
- We don't need interactive terminal features (user input, resize responses)
- We can add PTY mode later if needed

Alternative: Full PTY mode where the command runs inside the terminal. More powerful but requires reworking `CommandExecutor`.

**Update**: Implemented with full PTY mode for accurate terminal emulation.

### Integration Point
Rather than modifying the tool framework, we'll use the existing `ToolOutputRendererRegistry` to register a custom renderer for `execute_command`. This keeps changes isolated and backwards-compatible.

**Update**: Went with a new `BlockData::TerminalBlock` variant for cleaner lifecycle management. The terminal entity needs to be created early (when the tool starts) and persist across re-renders, which fits better as a block type than a stateless renderer.

### Color Mapping
For ANSI-to-theme color mapping, we'll check if `gpui-component`'s theme provides terminal color tokens. If not, we'll define sensible defaults that work with both light and dark themes.

---

## Notes

- Zed uses a custom fork of `alacritty_terminal` (git rev `9d9640d4`). We should evaluate whether the crates.io version works or if we need the same fork.
- The `terminal` crate should NOT depend on `code_assistant` — it should be a generic, reusable terminal component.
- Zed's `terminal` crate is GPL, but we're writing our own code using `alacritty_terminal` (Apache-2.0) directly, so no license issues.
