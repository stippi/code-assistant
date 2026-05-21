pub mod block_types;
pub mod container;

pub use block_types::*;
pub use container::*;

use crate::ui::gpui::file_icons;

use crate::ui::ToolStatus;
use gpui::{
    div, img, percentage, px, rems, svg, Animation, AnimationExt, ClickEvent, Context, Entity,
    ImageSource, IntoElement, ObjectFit, Pixels, SharedString, Styled, Task, Transformation,
};
use gpui::{prelude::*, FontWeight};
use gpui_component::{
    text::{TextView, TextViewState},
    ActiveTheme,
};

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Maximum height for rendered images in pixels
const MAX_IMAGE_HEIGHT: f32 = 80.0;

/// Role of a message in the conversation
#[derive(Debug, Clone, PartialEq)]
pub enum MessageRole {
    User,
    Assistant,
    /// System-level messages (e.g. compaction dividers) that have no author header
    System,
}

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

use crate::ui::gpui::ui_state::UiStateStore;

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
                if let Some(sender) = cx.try_global::<crate::ui::gpui::UiEventSender>() {
                    let _ = sender.0.try_send(crate::ui::UiEvent::PersistUiState);
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
                if let Some(sender) = cx.try_global::<crate::ui::gpui::UiEventSender>() {
                    let _ = sender.0.try_send(crate::ui::UiEvent::PersistUiState);
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

    /// Render a zigzag/wiggle line using a canvas element.
    /// The line fills the available width and is vertically centered.
    fn render_zigzag_line(color: gpui::Hsla) -> impl IntoElement {
        use gpui::{canvas, point, PathBuilder};

        canvas(
            |_, _, _| {},
            move |bounds, _, window, _cx| {
                let width = bounds.size.width;
                let height = bounds.size.height;
                let y_center = bounds.origin.y + height / 2.0;
                let x_start = bounds.origin.x;

                // Zigzag parameters
                let segment_width_f = 6.0_f32;
                let amplitude = px(2.5);

                // Compute number of segments from the width (Pixels -> f32 via division trick)
                // width / px(1.0) isn't available, so we'll use a large fixed count
                // and clamp x positions to not exceed bounds.
                let approx_segments = 200_i32; // More than enough for any realistic width

                let mut builder = PathBuilder::stroke(px(1.0));
                builder.move_to(point(x_start, y_center));

                for i in 1..=approx_segments {
                    let x = x_start + px(segment_width_f * i as f32);
                    if x > x_start + width {
                        break;
                    }
                    let y = if i % 2 == 0 {
                        y_center - amplitude
                    } else {
                        y_center + amplitude
                    };
                    builder.line_to(point(x, y));
                }

                if let Ok(path) = builder.build() {
                    window.paint_path(path, color);
                }
            },
        )
        .size_full()
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

    // ------------------------------------------------------------------
    // Card skeleton (shown while parameters are still streaming)
    // ------------------------------------------------------------------

    /// Render a minimal card header for a tool whose renderer returned `None`
    /// (typically because parameters haven't arrived yet). This prevents the
    /// ugly `[edit]` / `[spawn_agent]` text flash.
    fn render_card_skeleton(
        &self,
        block: &ToolUseBlock,
        renderer: &dyn crate::ui::gpui::tool_cards::ToolBlockRenderer,
        theme: &gpui_component::theme::Theme,
    ) -> gpui::AnyElement {
        let is_dark = theme.background.l < 0.5;
        let header_bg = if is_dark {
            gpui::hsla(0.0, 0.0, 0.15, 1.0)
        } else {
            gpui::hsla(0.0, 0.0, 0.93, 1.0)
        };
        let header_text_color = theme.muted_foreground;
        let icon = file_icons::get().get_tool_icon(&block.name);
        let label = renderer.describe(block);

        div()
            .w_full()
            .border_1()
            .border_color(theme.border)
            .rounded_md()
            .overflow_hidden()
            .child(
                div()
                    .px_3()
                    .py_1p5()
                    .bg(header_bg)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1p5()
                    .child(file_icons::render_icon_container(
                        &icon,
                        13.0,
                        header_text_color,
                        "⚙",
                    ))
                    .child(
                        div()
                            .text_size(rems(0.75))
                            .text_color(header_text_color)
                            .child(label),
                    ),
            )
            .into_any_element()
    }

    // ------------------------------------------------------------------
    // Inline tool rendering
    // ------------------------------------------------------------------

    /// Render a tool block in the compact inline style.
    ///
    /// Layout:
    /// ```text
    /// [icon]  Description text                          [▾]   (chevron on hover)
    /// │  output content when expanded …
    /// ```
    fn render_inline_tool(
        &mut self,
        block: &ToolUseBlock,
        renderer: &dyn crate::ui::gpui::tool_cards::ToolBlockRenderer,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let theme = cx.theme().clone();

        // Icon
        let icon = file_icons::get().get_tool_icon(&block.name);
        let (icon_color, desc_color) = match block.status {
            ToolStatus::Error => (theme.danger, theme.danger),
            ToolStatus::Running | ToolStatus::Pending | ToolStatus::Success => {
                (theme.muted_foreground, theme.muted_foreground)
            }
        };

        // Description text
        let description = if block.status == ToolStatus::Error {
            if let Some(ref msg) = block.status_message {
                format!("{} — {}", renderer.describe(block), msg)
            } else {
                renderer.describe(block)
            }
        } else {
            renderer.describe(block)
        };

        // Determine expansion state — purely based on ToolBlockState, no is_generating override
        let is_expanded = block.state == ToolBlockState::Expanded;
        let has_output =
            block.output.as_ref().is_some_and(|o| !o.is_empty()) || !block.images.is_empty();
        let can_expand = has_output;

        // Animation scale for smooth expand/collapse
        let animation_scale = match &self.animation_state {
            AnimationState::Animating { height_scale, .. } => *height_scale,
            AnimationState::Idle => {
                if is_expanded {
                    1.0
                } else {
                    0.0
                }
            }
        };

        // Chevron icon (only visible on hover, via group)
        let chevron_icon = if is_expanded {
            file_icons::get().get_type_icon(file_icons::CHEVRON_UP)
        } else {
            file_icons::get().get_type_icon(file_icons::CHEVRON_DOWN)
        };
        let chevron_color = theme.muted_foreground;

        // Running spinner
        let show_spinner = self.is_generating
            && (block.status == ToolStatus::Pending || block.status == ToolStatus::Running);

        // --- Build the element ---
        let mut container = div().w_full().mt_0p5();

        // Header line: clickable area with icon + description + chevron-on-hover
        let header = div()
            .id("inline-tool-header")
            .group("inline-tool")
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_1()
            .py_1p5()
            .px_3()
            .cursor_pointer()
            .when(!can_expand && !is_expanded, |d| d.cursor_default())
            .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                view.toggle_tool_collapsed(cx);
            }))
            .child(
                // Left side: icon + description
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1p5()
                    .flex_grow()
                    .min_w_0()
                    // Icon (or spinner) — both wrapped in a 14×14 container
                    // to prevent layout shift when transitioning.
                    .when(show_spinner, |d| {
                        d.child(
                            div()
                                .w(px(14.))
                                .h(px(14.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    gpui::svg()
                                        .size(px(14.))
                                        .path(SharedString::from("icons/arrow_circle.svg"))
                                        .text_color(icon_color)
                                        .with_animation(
                                            "inline_spinner",
                                            Animation::new(Duration::from_secs(2)).repeat(),
                                            |svg, delta| {
                                                svg.with_transformation(Transformation::rotate(
                                                    percentage(delta),
                                                ))
                                            },
                                        ),
                                ),
                        )
                    })
                    .when(!show_spinner, |d| {
                        d.child(file_icons::render_icon_container(
                            &icon, 14.0, icon_color, "🔧",
                        ))
                    })
                    // Description text
                    .child(
                        div()
                            .text_size(rems(0.8125))
                            .text_color(desc_color)
                            .overflow_hidden()
                            .text_overflow(gpui::TextOverflow::Truncate(SharedString::from("…")))
                            .child(description),
                    ),
            )
            // Chevron area — always laid out to prevent height changes when
            // output becomes available. The icon itself is only visible when
            // expandable, with a highlight on hover.
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(24.))
                    .rounded(px(6.))
                    .when(can_expand, |d| {
                        d.group_hover("inline-tool", |s| s.bg(theme.muted_foreground.opacity(0.1)))
                            .child(file_icons::render_icon(
                                &chevron_icon,
                                14.0,
                                chevron_color.opacity(0.4),
                                "▾",
                            ))
                    }),
            );

        container = container.child(header);

        // Animated output area
        if (is_expanded || animation_scale > 0.0) && has_output {
            if let Some(output_el) =
                renderer.render(block, self.is_generating, &theme, None, window, cx)
            {
                container = container.child(crate::ui::gpui::tool_cards::animated_card_body(
                    output_el,
                    animation_scale,
                    self.content_height.clone(),
                ));
            }
        }

        container
    }
}

impl Render for BlockView {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.block.clone() {
            BlockData::TextBlock(block) => div()
                .mt_3()
                .text_color(cx.theme().foreground)
                .child(self.markdown_view(&block.content, true, cx))
                .into_any_element(),
            BlockData::ThinkingBlock(block) => {
                // Get the appropriate icon based on completed state
                let (icon, icon_text) = if block.is_completed {
                    (
                        file_icons::get().get_type_icon(file_icons::WORKING_MEMORY),
                        "🧠",
                    )
                } else {
                    (Some(SharedString::from("icons/arrow_circle.svg")), "🔄")
                };

                // Get the chevron icon based on collapsed state
                let (chevron_icon, chevron_text) = if block.is_collapsed {
                    (
                        file_icons::get().get_type_icon(file_icons::CHEVRON_DOWN),
                        "▼",
                    )
                } else {
                    (file_icons::get().get_type_icon(file_icons::CHEVRON_UP), "▲")
                };

                // Define header text based on state using reasoning-aware method
                let header_text = block.get_display_title(self.is_generating);

                // Use theme utilities for colors
                let blue_base = cx.theme().info; // Theme color for thinking block
                let thinking_bg = crate::ui::gpui::theme::colors::thinking_block_bg(cx.theme());
                let chevron_color =
                    crate::ui::gpui::theme::colors::thinking_block_chevron(cx.theme());
                let text_color = cx.theme().info_foreground;

                div()
                    .mt_2()
                    .rounded_md()
                    .bg(thinking_bg)
                    .flex()
                    .flex_col()
                    .children(vec![
                        // Header row — entire row is clickable
                        div()
                            .id("thinking-header")
                            .group("thinking-header")
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .w_full()
                            .px_3()
                            .py_1p5()
                            .cursor_pointer()
                            .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                                view.toggle_thinking_collapsed(cx);
                            }))
                            .children(vec![
                                // Left side with icon and text
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .children(vec![
                                        // Rotating arrow or brain icon
                                        if block.is_completed {
                                            file_icons::render_icon_container(
                                                &icon, 18.0, blue_base, icon_text,
                                            )
                                            .into_any()
                                        } else {
                                            svg()
                                                .size(px(18.))
                                                .path(SharedString::from("icons/arrow_circle.svg"))
                                                .text_color(blue_base)
                                                .with_animation(
                                                    "image_circle",
                                                    Animation::new(Duration::from_secs(2)).repeat(),
                                                    |svg, delta| {
                                                        svg.with_transformation(
                                                            Transformation::rotate(percentage(
                                                                delta,
                                                            )),
                                                        )
                                                    },
                                                )
                                                .into_any()
                                        },
                                        // Header text
                                        div()
                                            .font_weight(FontWeight(500.0))
                                            .text_color(blue_base)
                                            .child(header_text)
                                            .into_any(),
                                    ])
                                    .into_any(),
                                // Chevron — highlights on header hover via group
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(24.))
                                    .rounded(px(6.))
                                    .group_hover("thinking-header", |s| {
                                        s.bg(blue_base.opacity(0.1))
                                    })
                                    .child(file_icons::render_icon(
                                        &chevron_icon,
                                        16.0,
                                        chevron_color,
                                        chevron_text,
                                    ))
                                    .into_any(),
                            ])
                            .into_any(),
                        // Animated content container (uses shared helper)
                        {
                            let scale = match &self.animation_state {
                                AnimationState::Animating { height_scale, .. } => *height_scale,
                                AnimationState::Idle => {
                                    if block.is_collapsed {
                                        0.0
                                    } else {
                                        1.0
                                    }
                                }
                            };

                            let body_content = if !block.is_collapsed || scale > 0.0 {
                                let content = block.get_expanded_content(self.is_generating);
                                div()
                                    .px_3()
                                    .pt_1()
                                    .pb_2()
                                    .text_size(rems(0.875))
                                    .italic()
                                    .text_color(text_color)
                                    .child(self.markdown_view(&content, false, cx))
                                    .into_any()
                            } else {
                                div().into_any()
                            };

                            crate::ui::gpui::tool_cards::animated_card_body(
                                body_content,
                                scale,
                                self.content_height.clone(),
                            )
                            .into_any()
                        },
                    ])
                    .into_any_element()
            }
            BlockData::ToolUse(block) => {
                // Unified tool block rendering via ToolBlockRendererRegistry
                if let Some(registry) =
                    crate::ui::gpui::tool_cards::ToolBlockRendererRegistry::global()
                {
                    if let Some(renderer) = registry.get(&block.name) {
                        match renderer.style() {
                            crate::ui::gpui::tool_cards::ToolBlockStyle::Inline => {
                                let block_clone = block.clone();
                                return self
                                    .render_inline_tool(&block_clone, renderer.as_ref(), window, cx)
                                    .into_any_element();
                            }

                            crate::ui::gpui::tool_cards::ToolBlockStyle::Card => {
                                let block_clone = block.clone();
                                let theme = cx.theme().clone();

                                // Build animation context from BlockView state
                                let scale = match &self.animation_state {
                                    AnimationState::Animating { height_scale, .. } => *height_scale,
                                    AnimationState::Idle => match block.state {
                                        ToolBlockState::Collapsed => 0.0,
                                        ToolBlockState::Expanded => 1.0,
                                    },
                                };

                                let current_project = self.current_project.lock().unwrap().clone();
                                let markdown_state = self.markdown_state("", cx);

                                let card_ctx = crate::ui::gpui::tool_cards::CardRenderContext {
                                    animation_scale: scale,
                                    is_collapsed: block.state == ToolBlockState::Collapsed,
                                    content_height: self.content_height.clone(),
                                    current_project,
                                    write_file_diff_mode: self.write_file_diff_mode,
                                    markdown_state: Some(markdown_state),
                                };

                                if let Some(element) = renderer.render(
                                    &block_clone,
                                    self.is_generating,
                                    &theme,
                                    Some(&card_ctx),
                                    window,
                                    cx,
                                ) {
                                    return div().mt_2().child(element).into_any_element();
                                }
                                // Renderer returned None (e.g. parameters still
                                // streaming) — show a skeleton card with just
                                // the header so we don't flash a raw "[name]"
                                // placeholder.
                                return div()
                                    .mt_2()
                                    .child(self.render_card_skeleton(
                                        &block,
                                        renderer.as_ref(),
                                        &theme,
                                    ))
                                    .into_any_element();
                            }
                        }
                    } else {
                        tracing::warn!("No ToolBlockRenderer registered for tool '{}'", block.name);
                    }
                }

                div()
                    .mt_0p5()
                    .px_2()
                    .py_1()
                    .text_color(cx.theme().muted_foreground)
                    .text_size(rems(0.8125))
                    .child(format!("[{}]", block.name))
                    .into_any_element()
            }
            BlockData::CompactionSummary(block) => {
                let is_expanded = block.is_expanded;

                // Chevron icon
                let chevron_icon = if is_expanded {
                    file_icons::get().get_type_icon(file_icons::CHEVRON_UP)
                } else {
                    file_icons::get().get_type_icon(file_icons::CHEVRON_DOWN)
                };
                let zigzag_color = cx.theme().border;
                let label_color = cx.theme().muted_foreground;

                // Zigzag line element (canvas-drawn)
                let zigzag_left = Self::render_zigzag_line(zigzag_color);
                let zigzag_right = Self::render_zigzag_line(zigzag_color);

                let header = div()
                    .id("compaction-header")
                    .group("compaction")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .py_1p5()
                    .px_3()
                    .cursor_pointer()
                    .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                        view.toggle_compaction(cx);
                    }))
                    // Left zigzag line
                    .child(
                        div()
                            .flex_1()
                            .h(px(8.))
                            .overflow_hidden()
                            .child(zigzag_left),
                    )
                    // Center: icon + label
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1p5()
                            .flex_none()
                            .child(
                                svg()
                                    .size(px(14.))
                                    .path(SharedString::from("icons/clear.svg"))
                                    .text_color(label_color),
                            )
                            .child(
                                div()
                                    .text_size(rems(0.8125))
                                    .text_color(label_color)
                                    .child("Conversation compacted"),
                            ),
                    )
                    // Right zigzag line
                    .child(
                        div()
                            .flex_1()
                            .h(px(8.))
                            .overflow_hidden()
                            .child(zigzag_right),
                    )
                    // Chevron
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(24.))
                            .rounded(px(6.))
                            .group_hover("compaction", |s| {
                                s.bg(cx.theme().muted_foreground.opacity(0.1))
                            })
                            .child(file_icons::render_icon(
                                &chevron_icon,
                                14.0,
                                label_color.opacity(0.4),
                                "▾",
                            )),
                    );

                let mut container = div().mt_2().w_full().flex().flex_col();
                container = container.child(header);

                // Animated expand/collapse for the summary content
                let animation_scale = match &self.animation_state {
                    AnimationState::Animating { height_scale, .. } => *height_scale,
                    AnimationState::Idle => {
                        if is_expanded {
                            1.0
                        } else {
                            0.0
                        }
                    }
                };

                if is_expanded || animation_scale > 0.0 {
                    let body = div()
                        .px_3()
                        .pb_2()
                        .text_color(cx.theme().foreground)
                        .child(self.markdown_view(&block.summary, true, cx));

                    container = container.child(crate::ui::gpui::tool_cards::animated_card_body(
                        body,
                        animation_scale,
                        self.content_height.clone(),
                    ));
                }

                container.into_any_element()
            }
            BlockData::ImageBlock(block) => {
                if let Some(image) = &block.image {
                    div()
                        .mt_2()
                        .flex_none() // Don't grow or shrink
                        .child(
                            div()
                                .border_1()
                                .border_color(cx.theme().border)
                                .rounded_md()
                                .overflow_hidden()
                                .bg(cx.theme().popover)
                                .shadow_sm()
                                .child(
                                    img(ImageSource::Image(image.clone()))
                                        .max_h(px(MAX_IMAGE_HEIGHT)) // Use constant for max height
                                        .object_fit(ObjectFit::Contain), // Maintain aspect ratio
                                ),
                        )
                        .into_any_element()
                } else {
                    // Fallback to placeholder if image parsing failed
                    div()
                        .mt_2()
                        .flex_none()
                        .p_2()
                        .bg(cx.theme().warning.opacity(0.1))
                        .border_1()
                        .border_color(cx.theme().warning.opacity(0.3))
                        .rounded_md()
                        .flex()
                        .items_center()
                        .gap_2()
                        .max_w(px(200.0)) // Limit width of error message
                        .child(
                            div()
                                .text_color(cx.theme().warning_foreground)
                                .text_xs()
                                .child("⚠️"),
                        )
                        .child(
                            div()
                                .text_color(cx.theme().warning_foreground.opacity(0.8))
                                .text_xs()
                                .child(format!("Failed: {}", block.media_type)),
                        )
                        .into_any_element()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::BranchInfo;
    use crate::ui::ToolStatus;
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
