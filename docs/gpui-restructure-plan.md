# GPUI Code Restructure Plan

Status: **In Progress**

## Goals

1. Reorganize the flat `ui/gpui/` module into a feature-based directory structure
2. Add GPUI unit tests using the `#[gpui::test]` framework (red/green TDD)
3. Rename "chat" terminology to "session" in UI code to align with backend

## Target File Structure

```
crates/code_assistant/src/ui/gpui/
├── mod.rs                          (slim: module declarations, re-exports only)
│
├── app/                            (application shell & lifecycle)
│   ├── mod.rs                      (Gpui struct definition, new(), run_app())
│   ├── event_loop.rs              (process_ui_event_async - the big dispatcher)
│   ├── backend.rs                 (setup_backend_communication, handle_backend_response)
│   ├── drafts.rs                  (save/load/clear draft logic)
│   └── user_interface_impl.rs     (UserInterface trait implementation)
│
├── main_screen/                    (the main chat screen)
│   ├── mod.rs                      (MainScreen struct, new(), Render impl)
│   ├── animation.rs              (sidebar animation logic)
│   ├── project_dialog.rs         (add-project flow, NewProjectDialog)
│   └── status_popover.rs         (render_status_popover)
│
├── messages/                       (message list area)
│   ├── mod.rs                      (MessagesView struct, list management)
│   ├── scroll.rs                  (spring-damper auto-scroll, animation task)
│   ├── activity_indicator.rs     (braille spinner, pending message, rate-limit)
│   ├── message_item.rs           (render_message - single message row rendering)
│   └── branch_switcher.rs        (branch navigation bubble)
│
├── blocks/                         (message block data + rendering)
│   ├── mod.rs                      (BlockData enum, BlockView struct, Render dispatch)
│   ├── block_types.rs            (TextBlock, ThinkingBlock, ImageBlock, ToolUseBlock,
│   │                               ParameterBlock, CompactionSummaryBlock struct defs)
│   ├── container.rs              (MessageContainer + all mutation methods)
│   ├── animation.rs              (AnimationConfig, AnimationState, animation tasks)
│   ├── text_renderer.rs          (text block rendering with markdown)
│   ├── thinking_renderer.rs      (thinking block rendering with collapse/expand)
│   ├── image_renderer.rs         (image block rendering)
│   └── compaction_renderer.rs    (compaction summary rendering)
│
├── tool_cards/                     (tool block rendering system)
│   ├── mod.rs                      (ToolBlockRenderer trait, Registry, CardRenderContext)
│   ├── inline_renderer.rs        (InlineToolRenderer for read-only tools)
│   ├── animated_card.rs          (animated_card_body helper, shared card utilities)
│   ├── code_card.rs              (read_files, search_files renderer)
│   ├── diff_card.rs              (edit, replace_in_file, write_file, delete_files)
│   ├── terminal_card.rs          (execute_command renderer)
│   └── sub_agent_card.rs         (spawn_agent renderer)
│
├── project_sidebar/                (left sidebar)
│   ├── mod.rs                      (SessionSidebar struct, Render, update_sessions)
│   ├── session_item.rs           (SessionListItem component)
│   └── project_group.rs          (ProjectGroup, render_project_header, show_more)
│
├── input/                          (bottom input area)
│   ├── mod.rs                      (InputArea struct, events, Render impl)
│   ├── paste_handler.rs          (clipboard image paste logic)
│   ├── attachment.rs             (attachment preview component)
│   └── selectors.rs              (model_selector, sandbox_selector, worktree_selector)
│
├── settings_screen/               (keep as-is)
│   └── ...
│
├── terminal/                       (terminal execution infrastructure)
│   ├── mod.rs
│   ├── executor.rs               (terminal command executor)
│   └── pool.rs                    (terminal entity pool)
│
├── shared/                         (shared utilities)
│   ├── mod.rs
│   ├── assets.rs                  (rust_embed AssetSource)
│   ├── auto_scroll.rs            (reusable auto-scroll container)
│   ├── context_indicator.rs      (circular progress ring)
│   ├── file_icons.rs             (icon resolution system)
│   ├── image.rs                   (base64 image decoding)
│   ├── plan_banner.rs            (plan progress banner)
│   ├── theme.rs                   (color themes)
│   ├── settings.rs               (UiSettings persistence)
│   └── ui_state.rs               (per-session UI state persistence)
│
└── events.rs                       (UiEventSender, globals like WorktreeData, UiSettingsGlobal)
```

## Approach: Test-Driven Extraction

Each phase extracts one logical module. For each extraction:

1. **Red**: Write tests for the component in its new location (using `#[gpui::test]`)
2. **Green**: Move the code, make tests pass
3. **Refactor**: Clean up imports, verify `cargo check` and `cargo test`

This ensures we never break existing functionality and end up with test coverage
for the newly independent modules.

## GPUI Test Patterns Reference

Tests use `#[gpui::test]` which provides `&mut TestAppContext`. Key patterns:

```rust
#[gpui::test]
fn test_example(cx: &mut TestAppContext) {
    // Open a window with a view
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|cx| MyView::new(cx))
        }).unwrap()
    });

    // Read state
    window.update(cx, |view, _, _| {
        assert_eq!(view.some_field, expected);
    }).unwrap();

    // Simulate input
    cx.simulate_keystrokes("enter");
    cx.dispatch_action(*window, MyAction);

    // Check events emitted
    let mut events = cx.events(&entity);
}
```

Components should be testable in isolation - extract dependencies behind traits
or pass them as constructor parameters.

## Phases

### Phase 0: Test Infrastructure Setup
- [ ] Add `gpui` test feature / dev-dependency if needed
- [ ] Create `src/ui/gpui/tests/` module with a minimal "hello world" gpui test
- [ ] Verify `cargo test --package code-assistant` runs the gpui test

### Phase 1: Extract `shared/` utilities (low coupling, easiest to test)
- [ ] Extract `theme.rs` → `shared/theme.rs`
- [ ] Extract `settings.rs` → `shared/settings.rs`
- [ ] Extract `ui_state.rs` → `shared/ui_state.rs` + add tests for state persistence logic
- [ ] Extract `assets.rs` → `shared/assets.rs`
- [ ] Extract `image.rs` → `shared/image.rs`
- [ ] Extract `file_icons.rs` → `shared/file_icons.rs` + add tests for icon resolution
- [ ] Extract `context_indicator.rs` → `shared/context_indicator.rs`
- [ ] Extract `auto_scroll.rs` → `shared/auto_scroll.rs`
- [ ] Extract `plan_banner.rs` → `shared/plan_banner.rs`
- [ ] Verify: `cargo check`, `cargo test`

### Phase 2: Extract `blocks/` (data models + container logic)
- [ ] Write tests for `MessageContainer` mutations (add block, append text, update tool status, etc.)
- [ ] Extract block type structs → `blocks/block_types.rs`
- [ ] Extract `MessageContainer` → `blocks/container.rs`
- [ ] Extract animation logic → `blocks/animation.rs`
- [ ] Extract `BlockView` + `BlockData` → `blocks/mod.rs`
- [ ] Extract text rendering → `blocks/text_renderer.rs`
- [ ] Extract thinking rendering → `blocks/thinking_renderer.rs`
- [ ] Extract image rendering → `blocks/image_renderer.rs`
- [ ] Extract compaction rendering → `blocks/compaction_renderer.rs`
- [ ] Verify: `cargo check`, `cargo test`

### Phase 3: Extract `tool_cards/`
- [ ] Write tests for `ToolBlockRendererRegistry` (registration, lookup)
- [ ] Write tests for `InlineToolRenderer` description generation
- [ ] Extract trait + registry → `tool_cards/mod.rs`
- [ ] Extract `animated_card_body` → `tool_cards/animated_card.rs`
- [ ] Extract `InlineToolRenderer` → `tool_cards/inline_renderer.rs`
- [ ] Move `code_card_renderer.rs` → `tool_cards/code_card.rs`
- [ ] Move `diff_card_renderer.rs` → `tool_cards/diff_card.rs`
- [ ] Move `terminal_card_renderer.rs` → `tool_cards/terminal_card.rs`
- [ ] Move `sub_agent_card_renderer.rs` → `tool_cards/sub_agent_card.rs`
- [ ] Verify: `cargo check`, `cargo test`

### Phase 4: Extract `project_sidebar/` (rename chat → session)
- [ ] Write tests for `SessionListItem` (activity state display, event emission)
- [ ] Write tests for `ProjectGroup` (expand/collapse, show-more logic)
- [ ] Rename `ChatSidebar` → `SessionSidebar`, `ChatListItem` → `SessionListItem`
- [ ] Extract → `project_sidebar/mod.rs`, `session_item.rs`, `project_group.rs`
- [ ] Update all references from `chat_sidebar` to `project_sidebar`/`session_sidebar`
- [ ] Verify: `cargo check`, `cargo test`

### Phase 5: Extract `input/`
- [ ] Write tests for `InputArea` (submit event on Enter, cancel, edit mode toggle)
- [ ] Write tests for paste handling (image paste detection)
- [ ] Extract paste logic → `input/paste_handler.rs`
- [ ] Move `attachment.rs` → `input/attachment.rs`
- [ ] Consolidate selectors → `input/selectors.rs` (or keep as separate files in `input/`)
- [ ] Extract `InputArea` → `input/mod.rs`
- [ ] Verify: `cargo check`, `cargo test`

### Phase 6: Extract `messages/`
- [ ] Write tests for scroll behavior (follow-tail, scroll-to-bottom trigger)
- [ ] Write tests for message list splicing / reset
- [ ] Extract scroll logic → `messages/scroll.rs`
- [ ] Extract activity indicator → `messages/activity_indicator.rs`
- [ ] Extract message item rendering → `messages/message_item.rs`
- [ ] Move `branch_switcher.rs` → `messages/branch_switcher.rs`
- [ ] Extract `MessagesView` → `messages/mod.rs`
- [ ] Verify: `cargo check`, `cargo test`

### Phase 7: Extract `terminal/`
- [ ] Write tests for terminal pool (add/get/remove by key)
- [ ] Move `terminal_executor.rs` → `terminal/executor.rs`
- [ ] Move `terminal_pool.rs` → `terminal/pool.rs`
- [ ] Verify: `cargo check`, `cargo test`

### Phase 8: Extract `main_screen/` splits
- [ ] Write tests for sidebar animation (easing, state transitions)
- [ ] Extract animation → `main_screen/animation.rs`
- [ ] Move `new_project_dialog.rs` → `main_screen/project_dialog.rs`
- [ ] Extract status popover → `main_screen/status_popover.rs`
- [ ] Verify: `cargo check`, `cargo test`

### Phase 9: Extract `app/` (the big split of mod.rs)
- [ ] Write tests for draft management (save/load/clear round-trip)
- [ ] Extract `events.rs` (globals, UiEventSender)
- [ ] Extract `app/drafts.rs`
- [ ] Extract `app/user_interface_impl.rs`
- [ ] Extract `app/backend.rs`
- [ ] Extract `app/event_loop.rs`
- [ ] Slim down `app/mod.rs` to struct + new() + run_app()
- [ ] Update top-level `mod.rs` to only declare submodules and re-exports
- [ ] Verify: `cargo check`, `cargo test`, `cargo clippy`

### Phase 10: Final rename pass (chat → session)
- [ ] Grep for remaining "chat" references in UI code
- [ ] Rename types, variables, file references
- [ ] Update any documentation references
- [ ] Final: `cargo check --tests`, `cargo test`, `cargo clippy --all-targets`

## Notes

- Each phase should be a separate commit (or small group of commits) for easy review
- The order is chosen to minimize cascading import changes: start with leaves (shared utilities),
  work inward toward the core (app/)
- The `settings_screen/` subdirectory is already well-structured and not touched
- The `streaming/` module (under `ui/`, not `ui/gpui/`) is not part of this refactor
