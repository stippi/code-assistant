# Tool Block Rendering Redesign

**Goal**: Replace the current uniform tool block rendering with differentiated styles inspired by Zed's Agent Panel. Simple exploration tools render inline and unobtrusive; complex tools with meaningful output render as distinct cards.

## Current State

All tools share a single rendering path in `elements.rs` (`BlockData::ToolUse` branch):
- Rounded container with shadow, left status border, bold tool name header
- Inline parameter badges in header, full-width params in expandable body
- Plugin systems: `ParameterRendererRegistry` for custom param rendering, `ToolOutputRendererRegistry` for custom output rendering
- Uniform collapse/expand animation with footer "Collapse" bar

This creates visual noise — a `read_files` call gets the same prominence as an `execute_command`.

## Design Overview

### Two Rendering Modes

#### 1. Inline Tools (exploration/read-only)

Tools: `read_files`, `list_files`, `search_files`, `glob_files`, `web_search`, `web_fetch`, `perplexity_ask`

Visual style:
- Single line: icon + tool description text (e.g. "Reading src/main.rs:1-30")
- No border, no card background, no shadow — blends into the message flow
- **Always starts collapsed**, stays collapsed by default
- Chevron-down icon appears **only on hover** (right side)
- On expand: output shown below with a subtle left border (like a blockquote)
- **Success**: no special indicator — success is the expected state
- **Error**: red ✕ icon replaces the tool icon, error message shown inline
- **Running**: subtle spinner or pulsing opacity on the icon

The tool description is generated from parameters:
| Tool | Description Template |
|------|---------------------|
| `read_files` | "Reading {paths}" |
| `list_files` | "Listing {paths}" |
| `search_files` | "Searching for '{regex}'" |
| `glob_files` | "Matching '{pattern}'" |
| `web_search` | "Searching web for '{query}'" |
| `web_fetch` | "Fetching {url}" |
| `perplexity_ask` | "Asking Perplexity" |

#### 2. Card Tools (tools with meaningful visual output)

##### Terminal Card (`execute_command`)

Structure:
```
┌─────────────────────────────────────────────────┐
│ /Users/user/project              (12s)  [Stop]  │  ← header: CWD, elapsed time, stop button
│                                                  │
│ Running: cargo test --package foo 2>&1 | tail -5 │  ← command line in monospace
│                                                  │
│ ┃ test result: ok. 436 passed; 0 failed         │  ← terminal output (live, ANSI colors)
│ ┃ finished in 0.17s                              │
│ ┃ █                                              │
│                                              ▴   │  ← footer collapse (only when tall)
└─────────────────────────────────────────────────┘
```

Header details:
- CWD path (muted, small font, monospace) on the left
- Elapsed time on the right (shown after ~2 seconds, updates live)
- Stop button (red, icon only) while command is running — kills the PTY process
- Collapse chevron appears **on hover** (replaces or sits next to the time)

Body:
- Command line in monospace font, prefixed with subtle "Running:" or "$" label
- Terminal view (existing `TerminalElement` with ANSI color rendering)
- Grows with output, no max height initially

Footer collapse:
- When terminal output exceeds ~400px, a "Collapse" footer appears at the bottom (same as current design but only for tall content)

On error (non-zero exit):
- Border turns red/dashed
- Error badge in header shows exit code

On success (exit 0):
- Subtle green left border (or no change — success is expected)
- Auto-collapse after a short delay? (TBD)

##### Edit/Write Card (`edit`, `replace_in_file`, `write_file`, `delete_files`)

Structure:
```
┌─────────────────────────────────────────────────┐
│ ✎  Editing crates/terminal_view/src/lib.rs      │  ← header: icon + file path
│                                                  │
│   div().w_full().child(TerminalElement::new(     │  ← diff: green = added
│                                                  │
│   div().size_full().child(TerminalElement::new(  │  ← diff: red = removed
│                                              ▴   │
└─────────────────────────────────────────────────┘
```

Header:
- File type icon + "Editing {path}" or "Writing {path}" or "Deleting {path}"
- Collapse chevron on hover

Body:
- Diff view (existing `EditDiffRenderer` / `DiffParameterRenderer`)
- For `write_file` of new files: show the content (possibly truncated)

##### Sub-Agent Card (`spawn_agent`)

Keeps the current rendering (already has a custom output renderer). Could adopt the card border style for consistency.

## Architecture

### ToolBlockRenderer Trait

The current two-level plugin system (`ParameterRenderer` + `ToolOutputRenderer`) is replaced by a single `ToolBlockRenderer` trait that controls the **entire** rendering of a tool block.

```rust
enum ToolBlockStyle { Inline, Card }

trait ToolBlockRenderer: Send + Sync {
    fn supported_tools(&self) -> Vec<String>;
    fn style(&self) -> ToolBlockStyle;
    fn describe(&self, tool: &ToolUseBlock) -> String;
    fn render(
        &self,
        tool: &ToolUseBlock,
        is_generating: bool,
        theme: &Theme,
        card_ctx: Option<&CardRenderContext>,  // None for inline tools
        window: &mut Window,
        cx: &mut Context<BlockView>,
    ) -> Option<AnyElement>;
}
```

Card renderers receive a `CardRenderContext` containing animation state from `BlockView`:
```rust
struct CardRenderContext {
    animation_scale: f32,          // 0.0 collapsed → 1.0 expanded
    is_collapsed: bool,            // target state
    content_height: Rc<Cell<Pixels>>, // persistent height for animated body
}
```

### Registry

```rust
struct ToolBlockRendererRegistry {
    renderers: HashMap<String, Arc<dyn ToolBlockRenderer>>,
}
```

Tools without a registered renderer fall through to the legacy rendering path.

### Registered Renderers

| Renderer | Tools | Style |
|----------|-------|-------|
| `InlineToolRenderer` | `read_files`, `list_files`, `search_files`, `glob_files`, `web_search`, `web_fetch`, `perplexity_ask` | Inline |
| `TerminalCardRenderer` | `execute_command` | Card |
| `DiffCardRenderer` | `edit`, `replace_in_file`, `write_file`, `delete_files` | Card |
| `SubAgentCardRenderer` | `spawn_agent` | Card |

### Rendering Flow in `elements.rs`

```
BlockData::ToolUse(block) →
  registry.get_renderer(&block.name) →
    match renderer.style() {
      Inline => render_inline_tool(renderer, block, ...)
      Card   => {
                  build CardRenderContext from BlockView animation state
                  renderer.render(block, ..., Some(&card_ctx), ...)
                }
    }
```

### Card Collapse Animation

Card collapse/expand reuses `BlockView`'s existing animation infrastructure (the same system used by thinking blocks):

- **State**: `BlockView.animation_state` (`AnimationState::Idle` or `AnimationState::Animating { height_scale, target, start_time }`)
- **Height measurement**: `BlockView.content_height` — a persistent `Rc<Cell<Pixels>>` shared with the `animated_card_body()` wrapper via `CardRenderContext`
- **Frame driver**: `BlockView.animation_task` — a spawned foreground task that loops at ~120fps, advancing `height_scale` with ease-out cubic easing, calling `cx.notify()` each frame
- **Toggle**: Card header `on_click` uses `cx.listener(|view, ...| view.toggle_tool_collapsed(cx))` which flips `ToolBlockState`, sets up the animation, and starts the task
- **Body wrapper**: `animated_card_body(content, scale, content_height)` constrains height to `measured_height * scale` using `on_children_prepainted` for measurement

Card-style tools start with `ToolBlockState::Expanded`. The `has_custom_renderer` check in `elements.rs` recognizes both old `ToolOutputRendererRegistry` entries and new `ToolBlockRendererRegistry` card-style entries.

## Implementation Progress

### Phase 1: Inline Tool Rendering ✅

1. ✅ Defined `ToolBlockRenderer` trait and `ToolBlockRendererRegistry` in `tool_block_renderers.rs`
2. ✅ Implemented `render_inline_tool()` in `elements.rs` — single line with icon, description, hover chevron
3. ✅ Created `InlineToolRenderer` with `describe()` templates for all 7 inline tools
4. ✅ Wired up: tools with inline renderers go through the new path; all others keep existing rendering
5. ✅ Error state: red ✕ icon + inline error message
6. ✅ Expand/collapse: on expand, output shown below with left-border style

### Phase 2: Terminal Card ✅

1. ✅ Created `TerminalCardRenderer` implementing `ToolBlockRenderer` (`terminal_card_renderer.rs`)
2. ✅ Card header: terminal icon, CWD path, elapsed time, status text, stop button, chevron
3. ✅ Card body: command line in monospace (`$ command`, copy-on-hover) + `TerminalView` output
4. ✅ Collapse/expand via header click with smooth animated height transition
5. ✅ Stop button: sends Ctrl-C (ETX) to the PTY terminal
6. ✅ Session restoration: display-only terminal fallback + view cache for persisted output
7. ✅ Card dispatch added to `elements.rs` for `ToolBlockStyle::Card`
8. ✅ `ExecuteCommandOutputRenderer` deregistered from old `ToolOutputRendererRegistry`

### Phase 3: Diff Card ✅

Renamed from "Edit/Write Card" — covers all file-mutation tools.

1. ✅ Created `DiffCardRenderer` implementing `ToolBlockRenderer` (`diff_card_renderer.rs`)
2. ✅ Card header: file-type icon + path (abbreviated with `~/`), red ✕ on error, chevron
3. ✅ `edit` tool body: unified diff view via `similar` crate (red bg for deletions, green bg for additions, Zed-style colored row backgrounds)
4. ✅ `replace_in_file` tool body: SEARCH/REPLACE section parser with streaming support (shows partial sections during streaming, full diff after completion)
5. ✅ `write_file` tool body: all-green additions (full content as inserted lines)
6. ✅ `delete_files` tool body: all-red deletion rows showing file paths; handles both `path` (single) and `paths` (JSON array) parameters
7. ✅ Error output shown below diff content in danger color
8. ✅ Smooth collapse/expand animation via `BlockView` animation infrastructure

### Phase 4: Sub-Agent Card ✅

1. ✅ Created `SubAgentCardRenderer` implementing `ToolBlockRenderer` (`sub_agent_card_renderer.rs`)
2. ✅ Card header: icon + "Sub-agent", activity spinner (while running), cancel button, red ✕ on error, chevron
3. ✅ Card body: instructions (muted, truncated), tool call history with status icons, activity line, error/cancelled status, markdown response via `TextView`
4. ✅ Cancel button sends `UiEvent::CancelSubAgent` via `UiEventSender` global
5. ✅ Smooth collapse/expand animation via `BlockView` animation infrastructure

### Phase 5: Cleanup (TODO)

1. Migrate remaining tools to the new system (if any unregistered tools exist)
2. Remove old `ParameterRendererRegistry` and `ToolOutputRendererRegistry` (or keep as internal implementation details within card renderers)
3. Remove the generic tool block rendering code from `elements.rs`
4. Remove old renderer files: `spawn_agent_renderer.rs`, `edit_diff_renderer.rs`, `diff_renderer.rs`, etc.

## Key Files

| File | Role |
|------|------|
| `src/ui/gpui/tool_block_renderers.rs` | Trait, registry, `InlineToolRenderer`, `CardRenderContext`, `animated_card_body()` |
| `src/ui/gpui/terminal_card_renderer.rs` | Terminal card for `execute_command` |
| `src/ui/gpui/diff_card_renderer.rs` | Diff card for `edit`, `replace_in_file`, `write_file`, `delete_files` |
| `src/ui/gpui/sub_agent_card_renderer.rs` | Sub-agent card for `spawn_agent` |
| `src/ui/gpui/elements.rs` | Dispatch logic, `BlockView` animation infrastructure, `toggle_tool_collapsed()` |
| `src/ui/gpui/mod.rs` | Registry initialization, renderer registration |

## Visual Reference

### Inline tool (collapsed — default state)
```
  🔍  Reading crates/terminal_view/src/lib.rs:1-30
```

### Inline tool (hover — chevron appears)
```
  🔍  Reading crates/terminal_view/src/lib.rs:1-30                    ▾
```

### Inline tool (expanded)
```
  🔍  Reading crates/terminal_view/src/lib.rs:1-30                    ▴
  │ 1 | //! Terminal view crate
  │ 2 | //!
  │ 3 | //! This crate provides:
  │ ...
```

### Inline tool (error)
```
  ✕  Reading nonexistent.rs — file not found
```

### Terminal card
```
┌─────────────────────────────────────────────────────┐
│ ~/project                                (3s)  ■    │
│                                                     │
│ $ cargo test --package code-assistant 2>&1 | tail -5│
│                                                     │
│ test result: ok. 436 passed; 0 failed; 0 ignored    │
│ finished in 0.17s                                   │
└─────────────────────────────────────────────────────┘
```

### Diff card
```
┌─────────────────────────────────────────────────────┐
│ ✎  crates/terminal_view/src/lib.rs                  │
│                                                     │
│     div().w_full().child(TerminalElement::new(      │  green bg
│                                                     │
│     div().size_full().child(TerminalElement::new(   │  red bg
└─────────────────────────────────────────────────────┘
```

### Sub-agent card
```
┌─────────────────────────────────────────────────────┐
│ ⚙  Sub-agent          Executing tools…      Cancel  │
│                                                     │
│ Implement the feature as described...               │  instructions
│─────────────────────────────────────────────────────│
│ 🔧 read_files                                       │  tool history
│ 🔧 edit                                             │
│                                                     │
│ Final response text rendered as markdown...         │  response
└─────────────────────────────────────────────────────┘
```

## Known Constraints

- **Terminal card must use `overflow_hidden()` + `rounded_md()`** on the card wrapper for the `TerminalElement` to render correctly inside `Entity<TerminalView>`. This is a GPUI layout requirement discovered during implementation.
- The `TerminalElement`'s `content_lines()` now reads from the live grid cursor (fixed during implementation), which is needed for correct inline height sizing.
- **Card header rounding**: Header uses `rounded_md()` when body is fully collapsed (`scale <= 0.0`) and `rounded_t_md()` when body is visible, preventing inner backgrounds from bleeding past the card's border-radius. Body uses `rounded_b_md()`.
- **`FluentBuilder::map()`** is used instead of `.when()` for conditional rounding on `Stateful<Div>` elements, since `.when()` closures on generic types require explicit type annotations that clutter the code.
