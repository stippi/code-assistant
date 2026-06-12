pub mod container;
pub mod data;
mod render;

pub use container::*;
pub use data::*;

use gpui::prelude::*;
use gpui::{px, Context, Entity, Pixels, Task};
use gpui_component::text::{TextView, TextViewState};

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use code_assistant_core::ui::ui_events::MessageRole;

/// State of a tool block for rendering and interaction
#[derive(Debug, Clone, PartialEq)]
pub enum ToolBlockState {
    /// Tool is collapsed - show parameters and output but collapsed
    Collapsed,
    /// Tool is expanded - show all content expanded
    Expanded,
}

// ---------------------------------------------------------------------------
// Tool-block collapse state helpers
// ---------------------------------------------------------------------------

use crate::shared::ui_state::UiStateStore;

/// Convenience helpers for tool-block collapse state.
///
/// These delegate to the global [`UiStateStore`], which keeps an in-memory
/// cache per session and debounces writes to the per-session UI state file.
pub struct ToolCollapseState;

impl ToolCollapseState {
    /// Look up a previously stored collapse override for a tool in a session.
    pub fn get(session_id: &str, tool_id: &str) -> Option<ToolBlockState> {
        UiStateStore::try_global()?
            .lock()
            .ok()
            .and_then(|mut store| {
                store
                    .get_tool_collapsed(session_id, tool_id)
                    .map(|collapsed| {
                        if collapsed {
                            ToolBlockState::Collapsed
                        } else {
                            ToolBlockState::Expanded
                        }
                    })
            })
    }

    /// Record a collapse state override for a tool in a session.
    /// Returns `true` if the store was marked dirty (i.e. a save should be
    /// scheduled).
    pub fn set(session_id: &str, tool_id: &str, state: ToolBlockState) -> bool {
        let collapsed = matches!(state, ToolBlockState::Collapsed);
        if let Some(store) = UiStateStore::try_global() {
            if let Ok(mut store) = store.lock() {
                store.set_tool_collapsed(session_id, tool_id, collapsed);
                return true;
            }
        }
        false
    }

    /// Remove all overrides for a session (e.g. when it is deleted).
    pub fn remove_session(session_id: &str) {
        if let Some(store) = UiStateStore::try_global() {
            if let Ok(mut store) = store.lock() {
                store.remove_session(session_id);
            }
        }
    }
}

/// Convenience helpers for write_file diff mode state.
///
/// When a write_file tool overwrites an existing file, the card can show either
/// a unified diff or the plain new-file content.  This persists the user's
/// choice per tool block.
pub struct ToolDiffModeState;

impl ToolDiffModeState {
    /// Look up a previously stored diff mode override for a tool in a session.
    /// Returns `None` if no override exists (default = diff mode on).
    pub fn get(session_id: &str, tool_id: &str) -> Option<bool> {
        UiStateStore::try_global()?
            .lock()
            .ok()
            .and_then(|mut store| store.get_tool_diff_mode(session_id, tool_id))
    }

    /// Record a diff mode override for a tool in a session.
    /// Returns `true` if the store was marked dirty (i.e. a save should be
    /// scheduled).
    pub fn set(session_id: &str, tool_id: &str, diff_mode: bool) -> bool {
        if let Some(store) = UiStateStore::try_global() {
            if let Ok(mut store) = store.lock() {
                store.set_tool_diff_mode(session_id, tool_id, diff_mode);
                return true;
            }
        }
        false
    }
}

/// Animation configuration for expand/collapse
#[derive(Clone)]
pub struct AnimationConfig {
    /// Animation frame rate (in milliseconds per frame)
    pub frame_ms: u64,
    /// Animation duration in milliseconds
    pub duration_ms: f32,
}

impl Default for AnimationConfig {
    fn default() -> Self {
        Self {
            frame_ms: 8,        // ~120 FPS
            duration_ms: 300.0, // 300ms constant animation time
        }
    }
}

/// Animation state for expand/collapse
#[derive(Clone, Debug, PartialEq)]
enum AnimationState {
    Idle,
    Animating {
        height_scale: f32,
        target: f32, // 0.0 for collapsing, 1.0 for expanding
        start_time: std::time::Instant,
    },
}

/// Entity view for a block
pub struct BlockView {
    block: BlockData,
    request_id: u64,
    markdown_state: Option<Entity<TextViewState>>,
    is_generating: bool, // Universal generating state for all block types
    // Animation state
    animation_state: AnimationState,
    content_height: Rc<Cell<Pixels>>,

    animation_task: Option<Task<()>>,
    /// Current project for parameter filtering (used to detect cross-project tool calls)
    #[allow(dead_code)]
    current_project: Arc<Mutex<String>>,
    /// Session ID this block belongs to (for collapse-state persistence).
    session_id: Option<String>,
    /// For write_file tool blocks: whether to show the diff view (true) or the
    /// plain new-file view (false). Only relevant when original_content is available.
    pub write_file_diff_mode: bool,
}

impl BlockView {
    pub fn new(
        block: BlockData,
        _block_id: u64,
        request_id: u64,
        current_project: Arc<Mutex<String>>,
        session_id: Option<String>,
        _cx: &mut Context<Self>,
    ) -> Self {
        // Load persisted diff mode preference for write_file tool blocks.
        let write_file_diff_mode = if let Some(tool) = block.as_tool() {
            if tool.name == "write_file" {
                session_id
                    .as_deref()
                    .and_then(|sid| ToolDiffModeState::get(sid, &tool.id))
                    .unwrap_or(true) // default: show diff
            } else {
                true
            }
        } else {
            true
        };

        let initial_markdown = match &block {
            BlockData::TextBlock(block) => Some(block.content.clone()),
            BlockData::ThinkingBlock(block) => Some(block.content.clone()),
            BlockData::CompactionSummary(block) => Some(block.summary.clone()),
            BlockData::ToolUse(_) | BlockData::ImageBlock(_) => None,
        };
        let markdown_state =
            initial_markdown.map(|text| _cx.new(|cx| TextViewState::markdown(&text, cx)));

        Self {
            block,
            request_id,
            markdown_state,
            is_generating: true, // Default to generating when first created
            animation_state: AnimationState::Idle,
            content_height: Rc::new(Cell::new(px(0.0))),
            animation_task: None,
            current_project,
            session_id,
            write_file_diff_mode,
        }
    }

    fn markdown_state(&mut self, text: &str, cx: &mut Context<Self>) -> Entity<TextViewState> {
        let state = if let Some(state) = &self.markdown_state {
            state.clone()
        } else {
            let state = cx.new(|cx| TextViewState::markdown(text, cx));
            self.markdown_state = Some(state.clone());
            state
        };

        state.update(cx, |state, cx| {
            state.set_text(text, cx);
        });

        state
    }

    fn markdown_view(&mut self, text: &str, selectable: bool, cx: &mut Context<Self>) -> TextView {
        let state = self.markdown_state(text, cx);
        TextView::new(&state).selectable(selectable)
    }

    /// Check if this block is an image block
    pub fn is_image_block(&self) -> bool {
        matches!(self.block, BlockData::ImageBlock(_))
    }

    /// Set the generating state of this block
    pub fn set_generating(&mut self, generating: bool) {
        self.is_generating = generating;
    }

    /// Check if this block can toggle expansion
    pub fn can_toggle_expansion(&self) -> bool {
        match &self.block {
            BlockData::ToolUse(_) => true, // Tools can always toggle, even while generating
            BlockData::ThinkingBlock(_) => true,
            BlockData::CompactionSummary(_) => true,
            _ => false, // Other blocks don't have expansion
        }
    }

    fn toggle_thinking_collapsed(&mut self, cx: &mut Context<Self>) {
        let should_expand = if let Some(thinking) = self.block.as_thinking_mut() {
            thinking.is_collapsed = !thinking.is_collapsed;
            !thinking.is_collapsed
        } else {
            return;
        };
        self.start_expand_collapse_animation(should_expand, cx);
    }

    pub fn toggle_tool_collapsed(&mut self, cx: &mut Context<Self>) {
        // Check if we can toggle expansion
        if !self.can_toggle_expansion() {
            return;
        }

        let should_expand = if let Some(tool) = self.block.as_tool_mut() {
            match tool.state {
                ToolBlockState::Collapsed => {
                    tool.state = ToolBlockState::Expanded;
                    true
                }
                ToolBlockState::Expanded => {
                    tool.state = ToolBlockState::Collapsed;
                    false
                }
            }
        } else {
            return;
        };

        // Persist the new state in the global UI state store (in-memory +
        // debounced write to disk) so it survives session reconnects and app
        // restarts.
        if let (Some(session_id), Some(tool)) = (&self.session_id, self.block.as_tool_mut()) {
            if ToolCollapseState::set(session_id, &tool.id, tool.state.clone()) {
                // Schedule a debounced save
                if let Some(sender) = cx.try_global::<crate::UiEventSender>() {
                    let _ = sender
                        .0
                        .try_send(code_assistant_core::ui::UiEvent::PersistUiState);
                }
            }
        }

        self.start_expand_collapse_animation(should_expand, cx);
    }

    /// Toggle between diff view and plain new-file view for write_file tool blocks.
    pub fn toggle_write_file_diff_mode(&mut self, cx: &mut Context<Self>) {
        self.write_file_diff_mode = !self.write_file_diff_mode;

        // Persist the new state
        if let (Some(session_id), Some(tool)) = (&self.session_id, self.block.as_tool()) {
            if ToolDiffModeState::set(session_id, &tool.id, self.write_file_diff_mode) {
                // Schedule a debounced save
                if let Some(sender) = cx.try_global::<crate::UiEventSender>() {
                    let _ = sender
                        .0
                        .try_send(code_assistant_core::ui::UiEvent::PersistUiState);
                }
            }
        }

        cx.notify();
    }

    fn toggle_compaction(&mut self, cx: &mut Context<Self>) {
        if let Some(summary) = self.block.as_compaction_mut() {
            let should_expand = !summary.is_expanded;
            summary.is_expanded = should_expand;
            self.start_expand_collapse_animation(should_expand, cx);
        }
    }

    fn start_expand_collapse_animation(&mut self, should_expand: bool, cx: &mut Context<Self>) {
        let target = if should_expand { 1.0 } else { 0.0 };
        let now = std::time::Instant::now();

        // Update animation state
        match &self.animation_state.clone() {
            AnimationState::Animating {
                height_scale,
                target: current_target,
                ..
            } if *current_target != target => {
                // Reverse direction: keep current height_scale, but adjust start_time for smooth transition
                let current_progress = if target == 1.0 {
                    *height_scale
                } else {
                    1.0 - *height_scale
                };
                let adjusted_start_time =
                    now - std::time::Duration::from_millis((current_progress * 300.0) as u64);

                self.animation_state = AnimationState::Animating {
                    height_scale: *height_scale,
                    target,
                    start_time: adjusted_start_time,
                };
            }
            _ => {
                // Start new animation
                let initial_height_scale = if should_expand { 0.0 } else { 1.0 };
                self.animation_state = AnimationState::Animating {
                    height_scale: initial_height_scale,
                    target,
                    start_time: now,
                };
            }
        }

        // Start animation task if not already running
        if self.animation_task.is_none() {
            self.start_animation_task(cx);
        }
    }

    fn start_animation_task(&mut self, cx: &mut Context<Self>) {
        let config = AnimationConfig::default();
        let task = cx.spawn(async move |weak_entity, async_app_cx| {
            loop {
                async_app_cx
                    .background_executor()
                    .timer(Duration::from_millis(config.frame_ms))
                    .await;

                let should_continue = weak_entity.update(async_app_cx, |view, cx| {
                    view.update_animation(&config);

                    // Check if animation should continue
                    match &view.animation_state {
                        AnimationState::Idle => false,
                        _ => {
                            cx.notify();
                            true
                        }
                    }
                });

                if let Ok(should_continue) = should_continue {
                    if !should_continue {
                        // Animation finished, clean up task
                        let _ = weak_entity.update(async_app_cx, |view, _cx| {
                            view.animation_task = None;
                        });
                        break;
                    }
                } else {
                    // Entity was dropped, stop animation
                    break;
                }
            }
        });

        self.animation_task = Some(task);
    }

    fn update_animation(&mut self, config: &AnimationConfig) {
        match &mut self.animation_state {
            AnimationState::Animating {
                height_scale,
                target,
                start_time,
            } => {
                let elapsed = start_time.elapsed().as_millis() as f32;
                let progress = (elapsed / config.duration_ms).min(1.0);

                // Easing function (ease_out_cubic for smooth deceleration)
                let eased_progress = 1.0 - (1.0 - progress).powi(3);

                *height_scale = if *target == 1.0 {
                    eased_progress // Animate from 0.0 -> 1.0
                } else {
                    1.0 - eased_progress // Animate from 1.0 -> 0.0
                };

                // Stop when animation complete
                if progress >= 1.0 {
                    *height_scale = *target;
                    self.animation_state = AnimationState::Idle;
                }
            }
            AnimationState::Idle => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_assistant_core::persistence::BranchInfo;
    use code_assistant_core::ui::ToolStatus;
    use gpui::TestAppContext;

    /// Initialize globals needed for tests (theme).
    fn init_test_globals(cx: &mut gpui::App) {
        gpui_component::theme::init(cx);
    }

    /// Helper to create a MessageContainer entity for testing.
    /// MessageContainer doesn't implement Render so we use cx.new() directly.
    fn make_container(role: MessageRole, cx: &mut TestAppContext) -> Entity<MessageContainer> {
        cx.update(|cx| {
            init_test_globals(cx);
            cx.new(|cx| MessageContainer::with_role(role, cx))
        })
    }

    #[gpui::test]
    fn test_message_container_add_text_block(cx: &mut TestAppContext) {
        let container = make_container(MessageRole::Assistant, cx);

        cx.update(|cx| {
            container.update(cx, |container, cx| {
                assert!(container.is_empty());
                container.add_text_block("Hello world", cx);
                assert!(!container.is_empty());
                let elements = container.elements();
                assert_eq!(elements.len(), 1);
            });
        });
    }

    #[gpui::test]
    fn test_message_container_add_or_append_to_text_block(cx: &mut TestAppContext) {
        let container = make_container(MessageRole::Assistant, cx);

        cx.update(|cx| {
            container.update(cx, |container, cx| {
                // First call creates a new text block
                container.add_or_append_to_text_block("Hello", cx);
                assert_eq!(container.elements().len(), 1);

                // Second call appends to existing
                container.add_or_append_to_text_block(" world", cx);
                assert_eq!(container.elements().len(), 1);

                // Verify content was appended
                let elements = container.elements();
                let block = elements[0].read(cx);
                if let BlockData::TextBlock(text) = &block.block {
                    assert_eq!(text.content, "Hello world");
                } else {
                    panic!("Expected TextBlock");
                }
            });
        });
    }

    #[gpui::test]
    fn test_message_container_add_or_append_to_thinking_block(cx: &mut TestAppContext) {
        let container = make_container(MessageRole::Assistant, cx);

        cx.update(|cx| {
            container.update(cx, |container, cx| {
                container.add_or_append_to_thinking_block("Thinking...", cx);
                assert_eq!(container.elements().len(), 1);

                // Append more
                container.add_or_append_to_thinking_block(" more thoughts", cx);
                assert_eq!(container.elements().len(), 1);

                // Verify content
                let elements = container.elements();
                let block = elements[0].read(cx);
                if let BlockData::ThinkingBlock(thinking) = &block.block {
                    assert_eq!(thinking.content, "Thinking... more thoughts");
                    assert!(!thinking.is_completed);
                } else {
                    panic!("Expected ThinkingBlock");
                }
            });
        });
    }

    #[gpui::test]
    fn test_message_container_add_tool_use_block(cx: &mut TestAppContext) {
        let container = make_container(MessageRole::Assistant, cx);

        cx.update(|cx| {
            container.update(cx, |container, cx| {
                container.add_tool_use_block("read_files", "tool-1", cx);
                assert_eq!(container.elements().len(), 1);

                let elements = container.elements();
                let block = elements[0].read(cx);
                if let BlockData::ToolUse(tool) = &block.block {
                    assert_eq!(tool.name, "read_files");
                    assert_eq!(tool.id, "tool-1");
                    assert_eq!(tool.status, ToolStatus::Pending);
                    assert!(tool.parameters.is_empty());
                } else {
                    panic!("Expected ToolUse block");
                }
            });
        });
    }

    #[gpui::test]
    fn test_message_container_update_tool_status(cx: &mut TestAppContext) {
        let container = make_container(MessageRole::Assistant, cx);

        cx.update(|cx| {
            container.update(cx, |container, cx| {
                container.add_tool_use_block("edit", "tool-2", cx);

                let updated = container.update_tool_status(
                    "tool-2",
                    ToolStatus::Success,
                    Some("Done".to_string()),
                    Some("output text".to_string()),
                    None,
                    Some(1.5),
                    vec![],
                    cx,
                );
                assert!(updated);

                let elements = container.elements();
                let block = elements[0].read(cx);
                if let BlockData::ToolUse(tool) = &block.block {
                    assert_eq!(tool.status, ToolStatus::Success);
                    assert_eq!(tool.status_message, Some("Done".to_string()));
                    assert_eq!(tool.output, Some("output text".to_string()));
                    assert_eq!(tool.duration_seconds, Some(1.5));
                } else {
                    panic!("Expected ToolUse block");
                }
            });
        });
    }

    #[gpui::test]
    fn test_message_container_update_tool_status_nonexistent(cx: &mut TestAppContext) {
        let container = make_container(MessageRole::Assistant, cx);

        cx.update(|cx| {
            container.update(cx, |container, cx| {
                container.add_tool_use_block("edit", "tool-2", cx);

                // Try to update a non-existent tool
                let updated = container.update_tool_status(
                    "non-existent",
                    ToolStatus::Success,
                    None,
                    None,
                    None,
                    None,
                    vec![],
                    cx,
                );
                assert!(!updated);
            });
        });
    }

    #[gpui::test]
    fn test_message_container_add_or_update_tool_parameter(cx: &mut TestAppContext) {
        let container = make_container(MessageRole::Assistant, cx);

        cx.update(|cx| {
            container.update(cx, |container, cx| {
                container.add_tool_use_block("read_files", "tool-3", cx);

                // Add a parameter
                container.add_or_update_tool_parameter("tool-3", "path", "src/main.rs", cx);

                let elements = container.elements();
                let block = elements[0].read(cx);
                if let BlockData::ToolUse(tool) = &block.block {
                    assert_eq!(tool.parameters.len(), 1);
                    assert_eq!(tool.parameters[0].name, "path");
                    assert_eq!(tool.parameters[0].value, "src/main.rs");
                } else {
                    panic!("Expected ToolUse block");
                }

                // Append to existing parameter
                container.add_or_update_tool_parameter("tool-3", "path", "/extra", cx);

                let elements = container.elements();
                let block = elements[0].read(cx);
                if let BlockData::ToolUse(tool) = &block.block {
                    assert_eq!(tool.parameters.len(), 1);
                    assert_eq!(tool.parameters[0].value, "src/main.rs/extra");
                } else {
                    panic!("Expected ToolUse block");
                }
            });
        });
    }

    #[gpui::test]
    fn test_message_container_remove_blocks_with_request_id(cx: &mut TestAppContext) {
        let container = make_container(MessageRole::Assistant, cx);

        cx.update(|cx| {
            container.update(cx, |container, cx| {
                container.set_current_request_id(1);
                container.add_text_block("First", cx);

                container.set_current_request_id(2);
                container.add_text_block("Second", cx);
                container.add_tool_use_block("edit", "tool-x", cx);

                assert_eq!(container.elements().len(), 3);

                // Remove blocks from request 2
                container.remove_blocks_with_request_id(2, cx);
                assert_eq!(container.elements().len(), 1);

                // Verify remaining block is from request 1
                let elements = container.elements();
                let block = elements[0].read(cx);
                if let BlockData::TextBlock(text) = &block.block {
                    assert_eq!(text.content, "First");
                } else {
                    panic!("Expected TextBlock");
                }
            });
        });
    }

    #[gpui::test]
    fn test_message_container_finish_thinking_blocks(cx: &mut TestAppContext) {
        let container = make_container(MessageRole::Assistant, cx);

        cx.update(|cx| {
            container.update(cx, |container, cx| {
                // Add a thinking block
                container.add_or_append_to_thinking_block("Thought 1", cx);

                // Verify it's not completed
                let elements = container.elements();
                let block = elements[0].read(cx);
                if let BlockData::ThinkingBlock(thinking) = &block.block {
                    assert!(!thinking.is_completed);
                } else {
                    panic!("Expected ThinkingBlock");
                }

                // Adding a text block should finish the thinking block
                container.add_text_block("Response", cx);

                // Verify thinking block is now completed
                let elements = container.elements();
                let block = elements[0].read(cx);
                if let BlockData::ThinkingBlock(thinking) = &block.block {
                    assert!(thinking.is_completed);
                } else {
                    panic!("Expected ThinkingBlock");
                }
            });
        });
    }

    #[gpui::test]
    fn test_message_container_append_tool_output(cx: &mut TestAppContext) {
        let container = make_container(MessageRole::Assistant, cx);

        cx.update(|cx| {
            container.update(cx, |container, cx| {
                container.add_tool_use_block("execute_command", "tool-4", cx);

                // Append streaming output
                container.append_tool_output("tool-4", "line 1\n", cx);
                container.append_tool_output("tool-4", "line 2\n", cx);

                let elements = container.elements();
                let block = elements[0].read(cx);
                if let BlockData::ToolUse(tool) = &block.block {
                    assert_eq!(tool.output, Some("line 1\nline 2\n".to_string()));
                } else {
                    panic!("Expected ToolUse block");
                }
            });
        });
    }

    #[gpui::test]
    fn test_message_container_is_user_message(cx: &mut TestAppContext) {
        let container = make_container(MessageRole::User, cx);

        cx.update(|cx| {
            assert!(container.read(cx).is_user_message());
        });
    }

    #[gpui::test]
    fn test_message_container_node_id_and_branch_info(cx: &mut TestAppContext) {
        let container = make_container(MessageRole::User, cx);

        cx.update(|cx| {
            container.update(cx, |container, _cx| {
                assert!(container.node_id().is_none());
                assert!(container.branch_info().is_none());

                container.set_node_id(Some(42));
                assert_eq!(container.node_id(), Some(42));

                let info = BranchInfo {
                    parent_node_id: None,
                    active_index: 0,
                    sibling_ids: vec![42, 43],
                };
                container.set_branch_info(Some(info.clone()));
                assert_eq!(container.branch_info(), Some(info));
            });
        });
    }

    #[gpui::test]
    fn test_thinking_block_formatted_duration(cx: &mut TestAppContext) {
        cx.update(|_cx| {
            let thinking = ThinkingBlock {
                content: "test".to_string(),
                is_collapsed: false,
                is_completed: true,
                start_time: std::time::Instant::now(),
                end_time: std::time::Instant::now(),
                duration_seconds: Some(45.0),
                reasoning_summary_items: vec![],
                current_generating_title: None,
                current_generating_content: None,
            };
            assert_eq!(thinking.formatted_duration(), "45s");

            let thinking_long = ThinkingBlock {
                duration_seconds: Some(125.0),
                ..thinking.clone()
            };
            assert_eq!(thinking_long.formatted_duration(), "2m5s");
        });
    }

    #[gpui::test]
    fn test_thinking_block_reasoning_summary(cx: &mut TestAppContext) {
        cx.update(|_cx| {
            let mut thinking = ThinkingBlock::new(String::new());
            assert!(!thinking.is_reasoning_block());

            // Start a reasoning summary item
            thinking.start_reasoning_summary_item();
            assert!(thinking.is_reasoning_block());

            // Append content
            thinking.append_reasoning_summary_delta("**Plan**\n\nI will do something".to_string());
            assert_eq!(thinking.current_generating_title, Some("Plan".to_string()));

            // Complete reasoning
            thinking.complete_reasoning();
            assert!(thinking.current_generating_content.is_none());
            assert_eq!(thinking.reasoning_summary_items.len(), 1);
        });
    }
}
