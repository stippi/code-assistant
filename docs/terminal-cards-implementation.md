# Terminal Cards Implementation Plan

**Branch**: `feature/terminal-cards`
**Goal**: Render execute_command tool output as embedded terminal cards in the GPUI UI, with full ANSI color support, similar to Zed's Agent Panel.

## Architecture

Two new workspace crates plus a test app, consumed by `code_assistant`:

```
crates/
  terminal/          # Alacritty terminal wrapper, GPUI entity
  terminal_view/     # GPUI Element that renders the terminal grid
  terminal_test_app/ # Isolated GPUI test app for validation
  code_assistant/    # Existing: consumes both new crates
```

### Key Dependencies

| Crate | Depends On |
|-------|-----------|
| `terminal` | `alacritty_terminal` (Apache-2.0, Zed's fork rev 9d9640d4), `gpui` (for Entity/EventEmitter) |
| `terminal_view` | `terminal`, `gpui`, `gpui-component` |
| `code_assistant` | `terminal`, `terminal_view` (+ existing deps) |

### Reference Code
Zed's implementation in the `zed` project (checked out locally):
- `crates/terminal/src/terminal.rs` — Terminal entity, PTY wiring, content snapshots
- `crates/terminal_view/src/terminal_element.rs` — GPUI Element for grid rendering
- `crates/agent_ui/src/connection_view/thread_view.rs:4687-4970` — Card UI wrapper
- `crates/agent_ui/src/entry_view_state.rs:377-396` — Embedded mode setup

---

## Completed Phases

### Phase 1: `terminal` crate (DONE)
Minimal terminal entity that can run a command via PTY and provide content snapshots.
- `TerminalBuilder::new()` — creates real PTY via `tty::new()`, spawns `EventLoop`
- `TerminalBuilder::new_display_only()` — no PTY, output injected via `write_output()`
- `Terminal` GPUI entity with event processing, `TerminalContent` snapshots, resize handling
- `TerminalBounds` implements alacritty's `Dimensions` trait
- Event loop subscription via `cx.spawn()` async closure

### Phase 2: `terminal_view` crate (DONE)
GPUI Element that paints the terminal grid with colors.
- `TerminalElement`: `Element` impl with `request_layout`/`prepaint`/`paint` phases
- `layout_grid()`: Converts cells to `BatchedTextRun`s and `LayoutRect`s
- `convert_color()`: Maps ANSI colors (Named 16, Indexed 256, True Color) to GPUI `Hsla`
- `TerminalView`: GPUI entity with embedded mode (inline growth) vs scrollable
- `TerminalThemeColors`: Configurable theme color struct

### Phase 2.5: Test App (DONE)
Isolated GPUI test app validating the terminal rendering:
- Real PTY terminals running shell commands with ANSI colors
- Terminal cards with header (command, status) and embedded TerminalView body
- `TerminalPool`: owns terminal entities independently of views
- `TerminalCard`: view entity that attaches to a terminal from the pool
- Keyboard-driven attach/detach testing (D/A/N keys)
- Confirmed working: ANSI colors, 256-color palette, attach/detach lifecycle

---

## Phase 3: GPUI Integration (NEXT)

### Overview

Replace `DefaultCommandExecutor` with a `GpuiTerminalCommandExecutor` that runs commands in real PTY terminals. Terminal output appears as live terminal cards in the chat, with full ANSI rendering.

### 3.1 GpuiTerminalCommandExecutor

**What**: A new `CommandExecutor` implementation that creates real PTY terminals on the GPUI thread.

**Pattern**: Worker/channel bridge between Tokio async (where the agent runs) and the GPUI foreground thread (where terminal entities live). Same pattern as `ACPTerminalCommandExecutor` (`acp/terminal_executor.rs`).

**Flow**:
```
Agent (tokio) ──request──▶ channel ──▶ GPUI worker task
                                         │
                                         ├─ creates Terminal entity via TerminalBuilder::new()
                                         ├─ sends ToolTerminal fragment (tool_id → terminal_id mapping)
                                         ├─ subscribes to Terminal events
                                         ├─ streams output chunks back via callback
                                         │
                                         ▼ on exit
                                       reads terminal text, sends CommandOutput back
```

**Key details**:
- The worker task runs on the GPUI foreground thread (via `cx.spawn()`) so it can create entities
- Terminal entities are stored in a `TerminalPool` (global or per-session) keyed by a unique terminal ID
- The executor maps `(session_id, tool_id)` → `terminal_id` so the UI can find the right terminal for each tool block. Tool IDs are LLM-generated and not guaranteed unique across sessions, so the session ID is required as part of the key.
- Output is collected from the terminal on completion via `Terminal::get_content_text()` and returned as `CommandOutput` to the tool

**Files to create/modify**:
- `src/ui/gpui/terminal_executor.rs` — new file, the executor implementation
- `src/ui/gpui/terminal_pool.rs` — new file, global terminal pool
- `src/app/gpui.rs:92` — inject `GpuiTerminalCommandExecutor` instead of `DefaultCommandExecutor`
- `src/ui/backend.rs:440` — same injection point for subsequent agent runs

### 3.2 Tool Output Renderer Plugin for Terminal Cards

**What**: Update `ExecuteCommandOutputRenderer` to find the real PTY terminal instance and render it, instead of creating display-only terminals.

**How the renderer finds the right terminal**:
1. The executor sends a `DisplayFragment::ToolTerminal { tool_id, terminal_id }` when a terminal is created
2. The UI stores the `(session_id, tool_id) → terminal_id` mapping (this already flows through the system but is currently ignored — see `ui/gpui/mod.rs:1874-1881`). The session ID is included because tool IDs are LLM-generated and not guaranteed unique across sessions.
3. When the output renderer's `render()` is called with a `tool_id`, it looks up the `terminal_id` using `(session_id, tool_id)`, then looks up the `Entity<Terminal>` in the pool
4. It creates/reuses a `TerminalView` entity and returns it as the rendered element

**Lifecycle**:
- **Running command**: Renderer finds the live terminal in the pool, attaches a TerminalView. Live output is visible because the Terminal entity processes PTY events in real time.
- **Completed command (same session)**: Terminal still in pool, renderer attaches a view showing the final state.
- **Session switch away then back**: Terminal cards are destroyed (views dropped), but Terminal entities survive in the pool. On reconnect, new views are created and attach to the same terminals — they show the full content because the alacritty grid still has all the data.
- **Session restore from persistence**: See section 3.3 below.

**Files to modify**:
- `src/ui/gpui/terminal_output_renderer.rs` — rewrite to use pool lookup instead of display-only creation
- `src/ui/gpui/mod.rs:1874-1881` — handle `ToolTerminal` fragment: store `tool_id → terminal_id` mapping

### 3.3 Persistence for Completed Terminals

**Problem**: When restoring a session from disk, there are no live Terminal entities — we need to recreate them from persisted data.

**Approach**: For completed (inactive) terminals, the tool output text is already persisted in the message history as `ContentBlock::ToolResult { content }`. On session restore:

1. The output renderer's `render()` is called with the `tool_id` and the persisted `output` text
2. If no live terminal exists in the pool for that `tool_id`, fall back to creating a **display-only terminal** and feed the persisted text via `write_output()`
3. This gives us the same visual result — ANSI codes in the persisted text are rendered correctly by the alacritty emulator

This is exactly what the current `terminal_output_renderer.rs` already does, so the fallback path is already implemented. The only change is to **prefer** a live terminal from the pool when available.

**Edge case — restoring a running terminal**: If the app is restarted while a command was running, the terminal is gone. The persisted output text may be partial. The renderer shows whatever text was captured, with an appropriate status indicator.

### 3.4 Terminal Card UI

The card wrapper (header with command, status, border colors) is already implemented in the test app's `TerminalCard` struct and works well. To integrate:

- Extract the card rendering pattern into a reusable component (or inline in the output renderer)
- Header shows: `$ {command}` on the left, status text on the right
- Border color: gray while running, green on success (exit 0), red on failure
- Body: embedded `TerminalView` in inline mode (auto-growing height)
- Collapse/expand: standard div toggle, hide the terminal body when collapsed

**Files**:
- The card UI can live directly in `terminal_output_renderer.rs` or be extracted to `src/ui/gpui/terminal_card.rs`

### 3.5 Wiring Checklist

- [ ] **3.5.1** Create `terminal_pool.rs` — global `TerminalPool` (keyed by terminal_id, stores `Entity<Terminal>` + metadata) with a separate `(session_id, tool_id) → terminal_id` index
- [ ] **3.5.2** Create `terminal_executor.rs` — `GpuiTerminalCommandExecutor` with worker/channel bridge
- [ ] **3.5.3** Initialize the executor's GPUI worker during app startup (`app/gpui.rs`)
- [ ] **3.5.4** Inject `GpuiTerminalCommandExecutor` at both agent creation points
- [ ] **3.5.5** Handle `DisplayFragment::ToolTerminal` in `ui/gpui/mod.rs` — store `(session_id, tool_id) → terminal_id` mapping
- [ ] **3.5.6** Rewrite `ExecuteCommandOutputRenderer::render()` to prefer pool lookup, fall back to display-only
- [ ] **3.5.7** Terminal card header/border UI in the output renderer
- [ ] **3.5.8** Collapse/expand toggle for cards
- [ ] **3.5.9** Test: run agent, verify live terminal output appears in chat
- [ ] **3.5.10** Test: switch sessions, switch back, verify terminal cards restore
- [ ] **3.5.11** Test: restart app, load persisted session, verify completed terminal cards show output

### Phase 4: Polish & Edge Cases

- [ ] **4.1** Theme integration: map ANSI colors to the active gpui-component theme
- [ ] **4.2** Handle long output: switch to scrollable mode when output exceeds threshold
- [ ] **4.3** Handle terminal resize when the panel resizes
- [ ] **4.4** Truncation indicator for very long outputs
- [ ] **4.5** Copy button for command text
- [ ] **4.6** Ensure terminal UI works in both light and dark themes
- [ ] **4.7** Performance: verify smooth rendering with large outputs (1000+ lines)
- [ ] **4.8** Cleanup: ensure terminals are properly dropped when sessions end
- [ ] **4.9** Remove the display-only terminal creation path if no longer needed as primary path (keep as fallback for persistence restore)

---

## Key File References

### Existing code (to modify)
- `src/app/gpui.rs:92` — agent creation, executor injection
- `src/ui/backend.rs:440` — agent creation on user message, executor injection
- `src/ui/gpui/mod.rs:263-270` — renderer registration
- `src/ui/gpui/mod.rs:1874-1881` — `ToolTerminal` fragment handling (currently no-op)
- `src/ui/gpui/terminal_output_renderer.rs` — current display-only renderer (to be rewritten)
- `src/tools/impls/execute_command.rs:73-101` — `ToolOutputStreamer` callback (already sends `ToolTerminal` fragment)

### New code (to create)
- `src/ui/gpui/terminal_pool.rs` — global terminal pool
- `src/ui/gpui/terminal_executor.rs` — GPUI terminal command executor

### Reference implementations
- `src/acp/terminal_executor.rs` — ACP terminal executor (same pattern, RPC instead of local PTY)
- `crates/terminal/src/lib.rs` — terminal entity
- `crates/terminal_view/src/lib.rs` — GPUI element + view
- `crates/terminal_test_app/src/main.rs` — working test app with pool pattern

### Tool/UI pipeline
- `src/agent/runner.rs:2065-2074` — ToolContext creation with executor + tool_id
- `src/tools/impls/execute_command.rs:230-246` — streaming execution call
- `crates/command_executor/src/default_executor.rs:57-165` — DefaultCommandExecutor streaming
- `src/ui/gpui/elements.rs:1458-1469` — output renderer invocation point
- `src/ui/gpui/elements.rs:271-292` — tool block creation with name + id
