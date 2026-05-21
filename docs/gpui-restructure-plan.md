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

### Phase 0: Test Infrastructure Setup — DONE ✓
- [x] Add `gpui` test-support feature to dev-dependencies
- [x] Write minimal `#[gpui::test]` to verify framework works (in plan_banner)
- [x] Verify `cargo test --package code-assistant` runs the gpui test

### Phase 1: Extract `shared/` utilities — DONE ✓
- [x] Extract `theme.rs` → `shared/theme.rs`
- [x] Extract `settings.rs` → `shared/settings.rs`
- [x] Extract `ui_state.rs` → `shared/ui_state.rs` + add 10 tests for persistence logic
- [x] Extract `assets.rs` → `shared/assets.rs`
- [x] Extract `image.rs` → `shared/image.rs`
- [x] Extract `file_icons.rs` → `shared/file_icons.rs`
- [x] Extract `context_indicator.rs` → `shared/context_indicator.rs`
- [x] Extract `auto_scroll.rs` → `shared/auto_scroll.rs`
- [x] Extract `plan_banner.rs` → `shared/plan_banner.rs` + add 5 tests (3 gpui::test)
- [x] Verify: `cargo check`, `cargo test`, `cargo clippy`

### Phase 3: Extract `tool_cards/` — DONE ✓
- [x] Extract trait + registry → `tool_cards/mod.rs` (with existing tests migrated)
- [x] Extract `animated_card_body` → `tool_cards/animated_card.rs`
- [x] Extract `InlineToolRenderer` → `tool_cards/inline_renderer.rs`
- [x] Move `code_card_renderer.rs` → `tool_cards/code_card.rs`
- [x] Move `diff_card_renderer.rs` → `tool_cards/diff_card.rs`
- [x] Move `terminal_card_renderer.rs` → `tool_cards/terminal_card.rs`
- [x] Move `sub_agent_card_renderer.rs` → `tool_cards/sub_agent_card.rs`
- [x] Verify: `cargo check`, `cargo test`, `cargo clippy`

### Phase 4: Extract `project_sidebar/` (rename chat → session) — DONE ✓
- [x] Rename `ChatSidebar` → `SessionSidebar`, `ChatListItem` → `SessionListItem`
- [x] Rename `ChatSidebarEvent` → `SessionSidebarEvent`, `ChatListItemEvent` → `SessionListItemEvent`
- [x] Move `chat_sidebar.rs` → `project_sidebar/mod.rs`
- [x] Update all references from `chat_sidebar` to `project_sidebar`
- [x] Fix comments referencing "chat sidebar"
- [x] Verify: `cargo check`, `cargo test`, `cargo clippy`

### Phase 5: Extract `input/` — DONE ✓
- [x] Move `input_area.rs` → `input/mod.rs`
- [x] Move `attachment.rs` → `input/attachment.rs`
- [x] Move `model_selector.rs` → `input/model_selector.rs`
- [x] Move `sandbox_selector.rs` → `input/sandbox_selector.rs`
- [x] Move `worktree_selector.rs` → `input/worktree_selector.rs`
- [x] Fix `super::` references for new nesting depth
- [x] Verify: `cargo check`, `cargo test`, `cargo clippy`

### Phase 7: Extract `terminal/` — DONE ✓
- [x] Move `terminal_executor.rs` → `terminal/executor.rs`
- [x] Move `terminal_pool.rs` → `terminal/pool.rs`
- [x] Fix internal references
- [x] Verify: `cargo check`, `cargo test`

### Phase 6: Extract `messages/` — DONE ✓
- [x] Write tests for scroll behavior (follow-tail, scroll-to-bottom trigger)
- [x] Write tests for message list splicing / reset
- [x] Extract scroll logic → `messages/scroll.rs`
- [x] Extract activity indicator → `messages/activity_indicator.rs`
- [x] Extract message item rendering → `messages/message_item.rs`
- [x] Move `branch_switcher.rs` → `messages/branch_switcher.rs`
- [x] Extract `MessagesView` → `messages/mod.rs`
- [x] Verify: `cargo check`, `cargo test`

### Phase 2: Extract `blocks/` (data models + container logic) — DONE ✓
- [x] Write tests for `MessageContainer` mutations (add block, append text, update tool status, etc.)
- [x] Move `elements.rs` → `blocks/mod.rs` with re-export shim
- [x] Extract block type structs → `blocks/block_types.rs`
- [x] Extract `MessageContainer` → `blocks/container.rs`
- [x] `BlockView` + animation + Render impl remain in `blocks/mod.rs` (tightly coupled)
- [x] Verify: `cargo check`, `cargo test`, `cargo clippy`

### Phase 8: Extract `main_screen/` splits — DONE ✓
- [x] Move `new_project_dialog.rs` → `main_screen/project_dialog.rs`
- [x] Extract status popover → `main_screen/status_popover.rs`
- [x] Sidebar animation kept in mod.rs (tightly coupled to MainScreen fields)
- [x] Verify: `cargo check`, `cargo test`, `cargo clippy`

### Phase 9: Extract `app/` (the big split of mod.rs) — DONE ✓
- [x] Extract `event_loop.rs` (process_ui_event_async, ~1240 lines)
- [x] Extract `backend.rs` (handle_backend_response, ~320 lines)
- [x] Extract `user_interface_impl.rs` (UserInterface trait impl, ~170 lines)
- [x] mod.rs reduced from 2601 → 867 lines
- [x] Remaining in mod.rs: struct definitions, new(), run_app(), helper methods, getters, drafts
- [x] Verify: `cargo check`, `cargo test`, `cargo clippy`

### Phase 10: Final rename pass (chat → session) — TODO
- [ ] Grep for remaining "chat" references in UI code (fields in Gpui struct, method names)
- [ ] Rename `chat_sessions` field → `sessions` in Gpui struct
- [ ] Rename `get_chat_sessions()` → `get_sessions()`
- [ ] Rename `UpdateChatList` event → `UpdateSessionList`
- [ ] Update any documentation references
- [ ] Final: `cargo check --tests`, `cargo test`, `cargo clippy --all-targets`

## Notes

- Each phase should be a separate commit (or small group of commits) for easy review
- The order is chosen to minimize cascading import changes: start with leaves (shared utilities),
  work inward toward the core (app/)
- The `settings_screen/` subdirectory is already well-structured and not touched
- The `streaming/` module (under `ui/`, not `ui/gpui/`) is not part of this refactor
- Phases 6, 2, 8, and 9 are the remaining heavy-lifting phases that involve splitting
  large files (elements.rs at 2324 lines, messages.rs at 1009 lines, mod.rs at 2602 lines)

## Current File Tree (after Phase 9)

```
crates/code_assistant/src/ui/gpui/
├── shared/                     ✓ DONE
│   ├── mod.rs
│   ├── assets.rs
│   ├── auto_scroll.rs
│   ├── context_indicator.rs
│   ├── file_icons.rs
│   ├── image.rs
│   ├── plan_banner.rs         (+ 5 tests)
│   ├── settings.rs
│   ├── theme.rs
│   └── ui_state.rs            (+ 10 tests)
├── tool_cards/                 ✓ DONE
│   ├── mod.rs                  (+ migrated tests)
│   ├── animated_card.rs
│   ├── code_card.rs
│   ├── diff_card.rs
│   ├── inline_renderer.rs
│   ├── sub_agent_card.rs
│   └── terminal_card.rs
├── project_sidebar/            ✓ DONE (renamed from chat_sidebar)
│   └── mod.rs
├── input/                      ✓ DONE
│   ├── mod.rs
│   ├── attachment.rs
│   ├── model_selector.rs
│   ├── sandbox_selector.rs
│   └── worktree_selector.rs
├── terminal/                   ✓ DONE
│   ├── mod.rs
│   ├── executor.rs
│   └── pool.rs
├── messages/                   ✓ DONE (+ 12 tests)
│   ├── mod.rs
│   ├── scroll.rs
│   ├── activity_indicator.rs
│   ├── message_item.rs
│   └── branch_switcher.rs
├── blocks/                     ✓ DONE (+ 14 tests)
│   ├── mod.rs                  (BlockView, AnimationConfig, ToolCollapseState, Render impl)
│   ├── block_types.rs          (TextBlock, ThinkingBlock, ImageBlock, ToolUseBlock, BlockData)
│   └── container.rs            (MessageContainer + all mutation methods)
├── main_screen/                ✓ DONE
│   ├── mod.rs                  (MainScreen struct, Render, event handlers)
│   ├── project_dialog.rs       (NewProjectDialog)
│   └── status_popover.rs       (error/status floating popover)
├── settings_screen/            (untouched)
│   └── ...
├── mod.rs                      (867 lines: Gpui struct, new(), run_app(), helpers, drafts)
├── event_loop.rs               (process_ui_event_async, ~1240 lines)
├── backend.rs                  (handle_backend_response, ~320 lines)
├── user_interface_impl.rs      (UserInterface trait impl, ~170 lines)
├── elements.rs                 (thin re-export shim → blocks/)
└── root.rs
```
