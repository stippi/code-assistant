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

### Replace Plugin Systems with a ToolBlockRenderer Trait

The current two-level plugin system (`ParameterRenderer` + `ToolOutputRenderer`) gets replaced by a single `ToolBlockRenderer` trait that controls the **entire** rendering of a tool block.

```rust
/// How a tool block should be rendered
enum ToolBlockStyle {
    /// Minimal inline rendering — icon + description text
    Inline,
    /// Full card with border, header, body
    Card,
}

trait ToolBlockRenderer: Send + Sync {
    /// Which tools this renderer handles
    fn supported_tools(&self) -> Vec<String>;

    /// Whether this tool renders as inline or card
    fn style(&self) -> ToolBlockStyle;

    /// Render the complete tool block.
    /// For Inline: returns the single-line element.
    /// For Card: returns the full card element.
    fn render(
        &self,
        tool: &ToolUseBlock,
        is_generating: bool,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<BlockView>,
    ) -> AnyElement;

    /// Generate a one-line description from parameters (for inline tools)
    fn describe(&self, tool: &ToolUseBlock) -> String {
        tool.name.clone()  // default: just the tool name
    }

    /// Whether this tool should auto-collapse on completion
    fn auto_collapse_on_success(&self) -> bool {
        true  // inline tools stay collapsed; cards may override
    }
}
```

### Registry

```rust
struct ToolBlockRendererRegistry {
    renderers: HashMap<String, Arc<dyn ToolBlockRenderer>>,
    default_inline_renderer: Arc<dyn ToolBlockRenderer>,
}
```

Tools without a registered renderer get the `default_inline_renderer`, which renders them inline with just `{tool_name} {params}`.

### Registered Renderers

| Renderer | Tools | Style |
|----------|-------|-------|
| `InlineReadRenderer` | `read_files`, `list_files` | Inline |
| `InlineSearchRenderer` | `search_files`, `glob_files` | Inline |
| `InlineWebRenderer` | `web_search`, `web_fetch`, `perplexity_ask` | Inline |
| `TerminalCardRenderer` | `execute_command` | Card |
| `EditCardRenderer` | `edit`, `replace_in_file`, `write_file`, `delete_files` | Card |
| `SubAgentCardRenderer` | `spawn_agent` | Card |

### Rendering Flow in `elements.rs`

```
BlockData::ToolUse(block) →
  registry.get_renderer(&block.name) →
    match renderer.style() {
      Inline => render_inline_tool(renderer, block, ...)
      Card   => render_card_tool(renderer, block, ...)
    }
```

The `render_inline_tool` and `render_card_tool` functions handle the shared chrome (hover behavior, collapse animation, error indicators) while delegating content to the renderer.

## Implementation Phases

### Phase 1: Inline Tool Rendering

Create the inline rendering path alongside the existing card rendering. This is the highest-impact visual change.

1. Define `ToolBlockRenderer` trait and `ToolBlockRendererRegistry`
2. Implement `render_inline_tool()` in `elements.rs` — single line with icon, description, hover chevron
3. Create `InlineReadRenderer` (for `read_files`, `list_files`) with `describe()` templates
4. Create `InlineSearchRenderer` (for `search_files`, `glob_files`)
5. Wire up: tools with inline renderers go through the new path; all others keep the existing rendering
6. Handle error state: red ✕ icon + inline error message
7. Handle expand/collapse: on expand, show output below with left-border style

### Phase 2: Terminal Card ✅

Redesign the `execute_command` rendering as a proper terminal card.

1. ✅ Create `TerminalCardRenderer` implementing `ToolBlockRenderer` (`terminal_card_renderer.rs`)
2. ✅ Card header: CWD (from working_dir param), terminal icon, elapsed time, stop button
3. ✅ Card body: command line in monospace (`$ command`) + `TerminalElement` output
4. ✅ Note: The terminal view rendering must keep `overflow_hidden` and the existing card structure (GPUI layout requirement discovered in this session)
5. ✅ Collapse/expand via header click with chevron icon
6. ✅ Stop button: sends Ctrl-C (ETX) to the PTY terminal
7. ✅ Card dispatch added to `elements.rs` for `ToolBlockStyle::Card`
8. ✅ `ExecuteCommandOutputRenderer` deregistered from old `ToolOutputRendererRegistry`

### Phase 3: Edit/Write Card

Redesign the edit tool rendering.

1. Create `EditCardRenderer` implementing `ToolBlockRenderer`
2. Card header: file icon + "Editing {path}"
3. Card body: diff view (reuse existing `EditDiffRenderer` logic)
4. Handle `write_file` (new file creation) vs `edit`/`replace_in_file` (modifications)

### Phase 4: Cleanup

1. Migrate remaining tools to the new system
2. Remove old `ParameterRendererRegistry` and `ToolOutputRendererRegistry` (or keep as internal implementation details within card renderers)
3. Remove the generic tool block rendering code from `elements.rs`
4. Unify collapse/expand animation across inline and card modes

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

### Edit card
```
┌─────────────────────────────────────────────────────┐
│ ✎  Editing crates/terminal_view/src/lib.rs          │
│                                                     │
│     div().w_full().child(TerminalElement::new(       │  green
│                                                     │
│     div().size_full().child(TerminalElement::new(    │  red
└─────────────────────────────────────────────────────┘
```

## Key Files to Modify

| File | Changes |
|------|---------|
| `src/ui/gpui/elements.rs` | New `render_inline_tool()` and `render_card_tool()` dispatch |
| `src/ui/gpui/tool_block_renderers.rs` | **New**: Trait + registry + inline renderers |
| `src/ui/gpui/terminal_card_renderer.rs` | **New**: Terminal card renderer |
| `src/ui/gpui/edit_card_renderer.rs` | **New**: Edit/write card renderer |
| `src/ui/gpui/mod.rs` | Register new renderers, phase out old registries |
| `src/ui/gpui/terminal_card_renderer.rs` | **New (Phase 2)**: Terminal card renderer (replaces terminal_output_renderer) |
| `src/ui/gpui/terminal_output_renderer.rs` | Deprecated (superseded by terminal_card_renderer.rs) |
| `src/ui/gpui/parameter_renderers.rs` | Deprecated (logic moves into card renderers) |
| `src/ui/gpui/tool_output_renderers.rs` | Deprecated (logic moves into card renderers) |

## Known Constraints

- **Terminal card must use `overflow_hidden()` + `rounded_md()`** on the card wrapper for the `TerminalElement` to render correctly inside `Entity<TerminalView>`. This is a GPUI layout requirement discovered during this session.
- The `TerminalElement`'s `content_lines()` now reads from the live grid cursor (fixed in this session), which is needed for correct inline height sizing.
