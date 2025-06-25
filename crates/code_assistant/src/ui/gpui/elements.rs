use crate::ui::gpui::file_icons;
use crate::ui::gpui::parameter_renderers::ParameterRendererRegistry;
use crate::ui::ToolStatus;
use gpui::{
    bounce, div, ease_in_out, percentage, px, svg, Animation, AnimationExt, Bounds, Context,
    Entity, IntoElement, MouseButton, Pixels, SharedString, Styled, Task, Timer, Transformation,
};
use gpui::{prelude::*, FontWeight};
use gpui_component::{label::Label, ActiveTheme};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::trace;

/// Role of a message in the conversation
#[derive(Debug, Clone, PartialEq)]
pub enum MessageRole {
    User,
    Assistant,
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

/// Container for all elements within a message
#[derive(Clone)]
pub struct MessageContainer {
    elements: Arc<Mutex<Vec<Entity<BlockView>>>>,
    role: MessageRole,
    /// The current_request_id is used to remove all blocks from a canceled request.
    /// The same MessageContainer may assemble blocks from multiple subsequent LLM responses.
    /// While the agent loop sends requests to the LLM provider, the request ID is updated for
    /// each new request (see `UiEvent::StreamingStarted` in gpui/mod). When the user cancels
    /// streaming, all blocks that were created for that last, canceled request are removed.
    current_request_id: Arc<Mutex<u64>>,
    /// Set to `true` in UiEvent::StreamingStarted, and set to false when the first streaming chunk
    /// is received. A progress spinner is showing while it is `true`.
    waiting_for_content: Arc<Mutex<bool>>,
    /// Rate limit countdown in seconds (None = no rate limiting)
    rate_limit_countdown: Arc<Mutex<Option<u64>>>,
}

impl MessageContainer {
    pub fn with_role(role: MessageRole, _cx: &mut Context<Self>) -> Self {
        Self {
            elements: Arc::new(Mutex::new(Vec::new())),
            role,
            current_request_id: Arc::new(Mutex::new(0)),
            waiting_for_content: Arc::new(Mutex::new(false)),
            rate_limit_countdown: Arc::new(Mutex::new(None)),
        }
    }

    // Set the current request ID for this message container
    pub fn set_current_request_id(&self, request_id: u64) {
        *self.current_request_id.lock().unwrap() = request_id;
    }

    // Set waiting for content flag
    pub fn set_waiting_for_content(&self, waiting: bool) {
        *self.waiting_for_content.lock().unwrap() = waiting;
    }

    // Check if waiting for content
    pub fn is_waiting_for_content(&self) -> bool {
        *self.waiting_for_content.lock().unwrap()
    }

    // Set rate limit countdown
    pub fn set_rate_limit_countdown(&self, seconds: Option<u64>) {
        *self.rate_limit_countdown.lock().unwrap() = seconds;
    }

    // Get rate limit countdown
    pub fn get_rate_limit_countdown(&self) -> Option<u64> {
        *self.rate_limit_countdown.lock().unwrap()
    }

    // Remove all blocks with the given request ID
    // Used when the user cancels a request while it is streaming
    pub fn remove_blocks_with_request_id(&self, request_id: u64, cx: &mut Context<Self>) {
        let mut elements = self.elements.lock().unwrap();
        let mut blocks_to_remove = Vec::new();

        // Find indices of blocks to remove
        for (index, element) in elements.iter().enumerate() {
            let should_remove = element.read(cx).request_id == request_id;
            if should_remove {
                blocks_to_remove.push(index);
            }
        }

        // Remove blocks in reverse order to maintain indices
        for &index in blocks_to_remove.iter().rev() {
            elements.remove(index);
        }

        if !blocks_to_remove.is_empty() {
            cx.notify();
        }
    }

    /// Check if this is a user message
    pub fn is_user_message(&self) -> bool {
        self.role == MessageRole::User
    }

    pub fn elements(&self) -> Vec<Entity<BlockView>> {
        let elements = self.elements.lock().unwrap();
        elements.clone()
    }

    // Add a new text block
    pub fn add_text_block(&self, content: impl Into<String>, cx: &mut Context<Self>) {
        self.finish_any_thinking_blocks(cx);

        // Clear waiting_for_content flag on first content
        self.set_waiting_for_content(false);

        let request_id = *self.current_request_id.lock().unwrap();
        let mut elements = self.elements.lock().unwrap();
        let block = BlockData::TextBlock(TextBlock {
            content: content.into(),
        });
        let view = cx.new(|cx| BlockView::new(block, request_id, cx));
        elements.push(view);
        cx.notify();
    }

    // Add a new thinking block
    #[allow(dead_code)]
    pub fn add_thinking_block(&self, content: impl Into<String>, cx: &mut Context<Self>) {
        self.finish_any_thinking_blocks(cx);

        // Clear waiting_for_content flag on first content
        self.set_waiting_for_content(false);

        let request_id = *self.current_request_id.lock().unwrap();
        let mut elements = self.elements.lock().unwrap();
        let block = BlockData::ThinkingBlock(ThinkingBlock::new(content.into()));
        let view = cx.new(|cx| BlockView::new(block, request_id, cx));
        elements.push(view);
        cx.notify();
    }

    // Add a new tool use block
    pub fn add_tool_use_block(
        &self,
        name: impl Into<String>,
        id: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.finish_any_thinking_blocks(cx);

        // Clear waiting_for_content flag on first content
        self.set_waiting_for_content(false);

        let request_id = *self.current_request_id.lock().unwrap();
        let mut elements = self.elements.lock().unwrap();
        let block = BlockData::ToolUse(ToolUseBlock {
            name: name.into(),
            id: id.into(),
            parameters: Vec::new(),
            status: ToolStatus::Pending,
            status_message: None,
            output: None,
            is_collapsed: false, // Default to expanded
        });
        let view = cx.new(|cx| BlockView::new(block, request_id, cx));
        elements.push(view);
        cx.notify();
    }

    // Update the status of a tool block
    pub fn update_tool_status(
        &self,
        tool_id: &str,
        status: ToolStatus,
        message: Option<String>,
        output: Option<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        let elements = self.elements.lock().unwrap();
        let mut updated = false;

        for element in elements.iter() {
            element.update(cx, |view, cx| {
                if let Some(tool) = view.block.as_tool_mut() {
                    if tool.id == tool_id {
                        tool.status = status.clone();
                        tool.status_message = message.clone();
                        tool.output = output.clone();

                        // Auto-collapse on completion or error
                        if status == ToolStatus::Success || status == ToolStatus::Error {
                            tool.is_collapsed = true;
                        } else {
                            // Ensure it's expanded if it's still pending or in progress
                            tool.is_collapsed = false;
                        }

                        updated = true;
                        cx.notify();
                    }
                }
            });
        }

        updated
    }

    // Add or append to text block
    pub fn add_or_append_to_text_block(&self, content: impl Into<String>, cx: &mut Context<Self>) {
        self.finish_any_thinking_blocks(cx);

        // Clear waiting_for_content flag on first content
        self.set_waiting_for_content(false);

        let content = content.into();
        let mut elements = self.elements.lock().unwrap();

        if let Some(last) = elements.last() {
            let mut was_appended = false;

            last.update(cx, |view, cx| {
                if let Some(text_block) = view.block.as_text_mut() {
                    text_block.content.push_str(&content);
                    was_appended = true;
                    cx.notify();
                }
            });

            if was_appended {
                return;
            }
        }

        // If we reach here, we need to add a new text block
        let request_id = *self.current_request_id.lock().unwrap();
        let block = BlockData::TextBlock(TextBlock {
            content: content.to_string(),
        });
        let view = cx.new(|cx| BlockView::new(block, request_id, cx));
        elements.push(view);
        cx.notify();
    }

    // Add or append to thinking block
    pub fn add_or_append_to_thinking_block(
        &self,
        content: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        // Clear waiting_for_content flag on first content
        self.set_waiting_for_content(false);

        let content = content.into();
        let mut elements = self.elements.lock().unwrap();

        if let Some(last) = elements.last() {
            let mut was_appended = false;

            last.update(cx, |view, cx| {
                if let Some(thinking_block) = view.block.as_thinking_mut() {
                    thinking_block.content.push_str(&content);
                    was_appended = true;
                    cx.notify();
                }
            });

            if was_appended {
                return;
            }
        }

        // If we reach here, we need to add a new thinking block
        let request_id = *self.current_request_id.lock().unwrap();
        let block = BlockData::ThinkingBlock(ThinkingBlock::new(content.to_string()));
        let view = cx.new(|cx| BlockView::new(block, request_id, cx));
        elements.push(view);
        cx.notify();
    }

    // Add or update tool parameter
    pub fn add_or_update_tool_parameter(
        &self,
        tool_id: impl Into<String>,
        name: impl Into<String>,
        value: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let tool_id = tool_id.into();
        let name = name.into();
        let value = value.into();
        let mut elements = self.elements.lock().unwrap();
        let mut tool_found = false;

        trace!(
            "Looking for tool_id: {}, param: {}, value len: {}",
            tool_id,
            name,
            value.len()
        );

        // Find the tool block with matching ID
        for element in elements.iter().rev() {
            let mut param_added = false;

            element.update(cx, |view, cx| {
                if let Some(tool) = view.block.as_tool_mut() {
                    if tool.id == tool_id {
                        tool_found = true;
                        trace!(
                            "Found tool: {}, current params: {}",
                            tool.name,
                            tool.parameters.len()
                        );

                        // Check if parameter with this name already exists
                        for param in tool.parameters.iter_mut() {
                            if param.name == name {
                                // Update existing parameter
                                param.value.push_str(&value);
                                trace!("Found param: {}, len now {}", name, param.value.len());
                                param_added = true;
                                break;
                            }
                        }

                        // Add new parameter if not found
                        if !param_added {
                            trace!("Adding param: {}, len {}", name, value.len());
                            tool.parameters.push(ParameterBlock {
                                name: name.clone(),
                                value: value.clone(),
                            });
                            param_added = true;
                        }

                        trace!("After update, params: {}", tool.parameters.len());
                        cx.notify();
                    }
                }
            });

            if param_added {
                return;
            }
        }

        // If we didn't find a matching tool, create a new one with this parameter
        if !tool_found {
            let request_id = *self.current_request_id.lock().unwrap();
            let mut tool = ToolUseBlock {
                name: "unknown".to_string(), // Default name since we only have ID
                id: tool_id.clone(),
                parameters: Vec::new(),
                status: ToolStatus::Pending,
                status_message: None,
                output: None,
                is_collapsed: false, // Default to expanded
            };

            tool.parameters.push(ParameterBlock {
                name: name.clone(),
                value: value.clone(),
            });

            let block = BlockData::ToolUse(tool);
            let view = cx.new(|cx| BlockView::new(block, request_id, cx));
            elements.push(view);
            cx.notify();
        }
    }

    // Mark a tool as ended (could add visual indicator)
    pub fn end_tool_use(&self, id: impl Into<String>, _cx: &mut Context<Self>) {
        // Currently no specific action needed, but could add visual indicator
        // that the tool execution is complete
        let _id = id.into();
    }

    fn finish_any_thinking_blocks(&self, cx: &mut Context<Self>) {
        let elements = self.elements.lock().unwrap();

        // Mark any previous thinking blocks as completed
        for element in elements.iter() {
            element.update(cx, |view, cx| {
                if let Some(thinking_block) = view.block.as_thinking_mut() {
                    if !thinking_block.is_completed {
                        thinking_block.is_completed = true;
                        thinking_block.end_time = std::time::Instant::now();
                        cx.notify();
                    }
                }
            });
        }
    }
}

/// Different types of blocks that can appear in a message
#[derive(Debug, Clone)]
pub enum BlockData {
    TextBlock(TextBlock),
    ThinkingBlock(ThinkingBlock),
    ToolUse(ToolUseBlock),
}

impl BlockData {
    fn as_text_mut(&mut self) -> Option<&mut TextBlock> {
        match self {
            BlockData::TextBlock(b) => Some(b),
            _ => None,
        }
    }

    fn as_thinking_mut(&mut self) -> Option<&mut ThinkingBlock> {
        match self {
            BlockData::ThinkingBlock(b) => Some(b),
            _ => None,
        }
    }

    fn as_tool_mut(&mut self) -> Option<&mut ToolUseBlock> {
        match self {
            BlockData::ToolUse(b) => Some(b),
            _ => None,
        }
    }
}

/// Entity view for a block
pub struct BlockView {
    block: BlockData,
    request_id: u64,
    // Animation state
    animation_state: AnimationState,
    content_height: Rc<Cell<Pixels>>,
    animation_task: Option<Task<()>>,
}

impl BlockView {
    pub fn new(block: BlockData, request_id: u64, _cx: &mut Context<Self>) -> Self {
        Self {
            block,
            request_id,
            animation_state: AnimationState::Idle,
            content_height: Rc::new(Cell::new(px(0.0))),
            animation_task: None,
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

    fn toggle_tool_collapsed(&mut self, cx: &mut Context<Self>) {
        let should_expand = if let Some(tool) = self.block.as_tool_mut() {
            tool.is_collapsed = !tool.is_collapsed;
            !tool.is_collapsed
        } else {
            return;
        };
        self.start_expand_collapse_animation(should_expand, cx);
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
            let mut timer = Timer::after(Duration::from_millis(config.frame_ms));

            loop {
                timer.await;
                timer = Timer::after(Duration::from_millis(config.frame_ms));

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

impl Render for BlockView {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.block {
            BlockData::TextBlock(block) => {
                // Use TextView with Markdown for rendering text
                div()
                    .text_color(cx.theme().foreground)
                    .child(gpui_component::text::TextView::markdown(
                        "md-block",
                        block.content.clone(),
                    ))
                    .into_any_element()
            }
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

                // Define header text based on state
                let header_text = if block.is_completed {
                    format!("Thought for {}", block.formatted_duration())
                } else {
                    "Thinking...".to_string()
                };

                // Use theme utilities for colors
                let blue_base = cx.theme().info; // Theme color for thinking block
                let thinking_bg = crate::ui::gpui::theme::colors::thinking_block_bg(&cx.theme());
                let chevron_color =
                    crate::ui::gpui::theme::colors::thinking_block_chevron(&cx.theme());
                let text_color = cx.theme().info_foreground;

                div()
                    .rounded_md()
                    .p_2()
                    .mb_2()
                    .bg(thinking_bg)
                    .flex()
                    .flex_col()
                    .children(vec![
                        // Header row with icon and text
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between() // Spread items
                            .w_full()
                            .mb_1()
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
                                            // Just render the brain icon normally
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
                                                    Animation::new(Duration::from_secs(2))
                                                        .repeat()
                                                        .with_easing(bounce(ease_in_out)),
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
                                // Right side with the expand/collapse button
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .size(px(24.))
                                    .rounded_full()
                                    .hover(|s| s.bg(blue_base.opacity(0.1)))
                                    .child(file_icons::render_icon(
                                        &chevron_icon,
                                        16.0,
                                        chevron_color,
                                        chevron_text,
                                    ))
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(move |view, _event, _window, cx| {
                                            view.toggle_thinking_collapsed(cx);
                                        }),
                                    )
                                    .into_any(),
                            ])
                            .into_any(),
                        // Animated content container
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

                            let content_height_rc = self.content_height.clone();

                            div()
                                .overflow_hidden()
                                .when(scale > 0.0, |div| {
                                    let actual_height = content_height_rc.get();
                                    let animated_height = actual_height * scale;
                                    div.h(animated_height)
                                })
                                .child(
                                    div()
                                        .on_children_prepainted({
                                            let content_height_rc = content_height_rc.clone();
                                            move |bounds_vec: Vec<Bounds<Pixels>>, _window, _app| {
                                                if let Some(first_child_bounds) = bounds_vec.first()
                                                {
                                                    let new_height = first_child_bounds.size.height;
                                                    if content_height_rc.get() != new_height {
                                                        content_height_rc.set(new_height);
                                                    }
                                                }
                                            }
                                        })
                                        .child(if !block.is_collapsed || scale > 0.0 {
                                            div()
                                                .pt_1()
                                                .text_size(px(14.))
                                                .italic()
                                                .text_color(text_color)
                                                .child(gpui_component::text::TextView::markdown(
                                                    "thinking-content",
                                                    block.content.clone(),
                                                ))
                                                .into_any()
                                        } else {
                                            // If collapsed, show a preview of the first line
                                            let first_line = block
                                                .content
                                                .lines()
                                                .next()
                                                .unwrap_or("")
                                                .to_string();
                                            div()
                                                .pt_1()
                                                .text_size(px(14.))
                                                .italic()
                                                .text_color(text_color)
                                                .opacity(0.7)
                                                .text_ellipsis()
                                                .child(gpui_component::text::TextView::markdown(
                                                    "thinking-preview",
                                                    first_line + "...",
                                                ))
                                                .into_any()
                                        }),
                                )
                                .into_any()
                        },
                    ])
                    .into_any_element()
            }
            BlockData::ToolUse(block) => {
                // Get the appropriate icon for this tool type
                let icon = file_icons::get().get_tool_icon(&block.name);

                // Get the chevron icon based on collapsed state
                let (chevron_icon, chevron_text) = if block.is_collapsed {
                    (
                        file_icons::get().get_type_icon(file_icons::CHEVRON_DOWN),
                        "▼",
                    )
                } else {
                    (file_icons::get().get_type_icon(file_icons::CHEVRON_UP), "▲")
                };

                // Use theme utilities for colors
                let icon_color =
                    crate::ui::gpui::theme::colors::tool_block_icon(&cx.theme(), &block.status);
                let tool_name_color =
                    crate::ui::gpui::theme::colors::tool_block_name(&cx.theme(), &block.status);
                let border_color = crate::ui::gpui::theme::colors::tool_border_by_status(
                    &cx.theme(),
                    &block.status,
                );
                let tool_bg = crate::ui::gpui::theme::colors::tool_block_bg(&cx.theme());
                let chevron_color = cx.theme().muted_foreground;

                // Parameter rendering function that uses the global registry if available
                let render_parameter = |param: &ParameterBlock| {
                    // Try to get the global registry
                    if let Some(registry) = ParameterRendererRegistry::global() {
                        // Use the registry to render the parameter with theme
                        registry.render_parameter(
                            &block.name,
                            &param.name,
                            &param.value,
                            &cx.theme(),
                        )
                    } else {
                        // Fallback to empty element
                        div().into_any_element()
                    }
                };

                // Separate parameters into regular and full-width
                let registry = ParameterRendererRegistry::global();

                let (regular_params, fullwidth_params): (
                    Vec<&ParameterBlock>,
                    Vec<&ParameterBlock>,
                ) = block.parameters.iter().partition(|param| {
                    !registry.as_ref().map_or(false, |reg| {
                        reg.get_renderer(&block.name, &param.name)
                            .is_full_width(&block.name, &param.name)
                    })
                });

                div()
                    .rounded(px(4.))
                    .my_2()
                    .bg(tool_bg)
                    .flex()
                    .flex_row()
                    .overflow_hidden()
                    .children(vec![
                        // Left side: Border with status indication
                        div()
                            .w(px(3.))
                            .flex_none()
                            .min_h_full()
                            .overflow_hidden()
                            // Use a child with enough width to avoid reducing the corner radius
                            .child(div().w(px(8.)).h_full().rounded(px(4.)).bg(border_color)),
                        div().flex_grow().min_w_0().child(
                            div().w_full().flex().flex_col().p_1().children({
                                let mut elements = Vec::new();

                                // First row: Tool header with icon, name, and regular parameters
                                elements.push(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .justify_between() // Space between header and chevron
                                        .cursor_pointer() // Make entire header clickable
                                        //.hover(|s| s.bg(border_color.opacity(0.1))) // Hover effect
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(move |view, _event, _window, cx| {
                                                view.toggle_tool_collapsed(cx);
                                            }),
                                        )
                                        .children(vec![
                                            // Left side: Tool icon, name and regular parameters
                                            div()
                                                .flex()
                                                .flex_row()
                                                .items_center()
                                                .flex_grow()
                                                .min_w_0() // Allow shrinking below content size
                                                .children(vec![
                                                    // Tool icon
                                                    file_icons::render_icon_container(
                                                        &icon, 16.0, icon_color, "🔧",
                                                    )
                                                    .mx_2()
                                                    .into_any(),
                                                    // Tool name
                                                    div()
                                                        .font_weight(FontWeight(700.0))
                                                        .text_color(tool_name_color)
                                                        .mr_2()
                                                        .flex_none() // Prevent shrinking
                                                        .child(block.name.clone())
                                                        .into_any(),
                                                    // Regular parameters
                                                    div()
                                                        .flex()
                                                        .flex_wrap()
                                                        .gap_1()
                                                        .flex_grow()
                                                        .min_w_0() // Allow shrinking and enable proper wrapping
                                                        .overflow_hidden() // Hide overflow instead of expanding
                                                        .children(
                                                            regular_params.iter().map(|param| {
                                                                render_parameter(param)
                                                            }),
                                                        )
                                                        .into_any(),
                                                ])
                                                .into_any(),
                                            // Right side: Chevron icon
                                            div()
                                                .mr_1()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .flex_none()
                                                .cursor_pointer()
                                                .size(px(24.))
                                                .rounded_full()
                                                .hover(|s| s.bg(border_color.opacity(0.2)))
                                                .child(file_icons::render_icon(
                                                    &chevron_icon,
                                                    16.0,
                                                    chevron_color,
                                                    chevron_text,
                                                ))
                                                .into_any(),
                                        ])
                                        .into_any(),
                                );

                                // Animated expandable content container
                                {
                                    let scale = match &self.animation_state {
                                        AnimationState::Animating { height_scale, .. } => *height_scale,
                                        AnimationState::Idle => if block.is_collapsed { 0.0 } else { 1.0 }
                                    };

                                    let content_height_rc = self.content_height.clone();
                                    let has_expandable_content = !fullwidth_params.is_empty() ||
                                        block.output.as_ref().map_or(false, |o| !o.is_empty());

                                    if has_expandable_content && (!block.is_collapsed || scale > 0.0) {
                                        elements.push(
                                            div()
                                                .overflow_hidden()
                                                .when(scale < 1.0, |div| {
                                                    // During animation, use measured height but prevent sudden changes
                                                    let actual_height = content_height_rc.get();
                                                    if actual_height > px(0.0) {
                                                        let animated_height = actual_height * scale;
                                                        div.h(animated_height)
                                                    } else {
                                                        div
                                                    }
                                                })
                                                .on_children_prepainted({
                                                    let content_height_rc = content_height_rc.clone();
                                                    move |bounds_vec: Vec<Bounds<Pixels>>, _window, _app| {
                                                        if let Some(first_child_bounds) = bounds_vec.first() {
                                                            let new_height = first_child_bounds.size.height;
                                                            if content_height_rc.get() != new_height {
                                                                content_height_rc.set(new_height);
                                                            }
                                                        }
                                                    }
                                                })
                                                .child(
                                                    div()
                                                        .flex()
                                                        .flex_col()
                                                        .children({
                                                            let mut expandable_elements = Vec::new();

                                                            // Full-width parameters
                                                            if !fullwidth_params.is_empty() {
                                                                expandable_elements.push(
                                                                    div()
                                                                        .flex()
                                                                        .flex_col()
                                                                        .w_full()
                                                                        .mt_1()
                                                                        .children(
                                                                            fullwidth_params
                                                                                .iter()
                                                                                .map(|param| render_parameter(param)),
                                                                        )
                                                                        .into_any(),
                                                                );
                                                            }

                                                            // Output with consistent rendering
                                                            if let Some(output_content) = &block.output {
                                                                if !output_content.is_empty() {
                                                                    let output_color =
                                                                        if block.status == crate::ui::ToolStatus::Error {
                                                                            cx.theme().danger
                                                                        } else {
                                                                            cx.theme().foreground
                                                                        };

                                                                    expandable_elements.push(
                                                                        div()
                                                                            .p_2()
                                                                            .mt_1()
                                                                            .w_full()
                                                                            .text_color(output_color)
                                                                            .text_size(px(13.))
                                                                            .whitespace_normal()
                                                                            .child(output_content.clone())
                                                                            .into_any(),
                                                                    );
                                                                }
                                                            }

                                                            expandable_elements
                                                        })
                                                )
                                                .into_any(),
                                        );
                                    }
                                }

                                // Error message (only shown for error status when collapsed, or when there's no output)
                                if block.status == crate::ui::ToolStatus::Error
                                    && block.status_message.is_some()
                                    && (block.is_collapsed
                                        || block.output.as_ref().map_or(true, |o| o.is_empty()))
                                {
                                    elements.push(
                                        div()
                                            .p_2()
                                            .mt_1()
                                            .text_color(cx.theme().danger.opacity(0.9))
                                            .text_size(px(13.))
                                            .whitespace_normal() // Allow text wrapping
                                            .child(block.status_message.clone().unwrap_or_default())
                                            .into_any(),
                                    );
                                }

                                // Bottom collapse bar with separate height and opacity animations
                                {
                                    let (scale, is_expanding) = match &self.animation_state {
                                        AnimationState::Animating { height_scale, target, .. } => (*height_scale, *target == 1.0),
                                        AnimationState::Idle => if block.is_collapsed { (0.0, false) } else { (1.0, false) }
                                    };

                                    if !fullwidth_params.is_empty()
                                        || block.output.as_ref().map_or(false, |o| !o.is_empty())
                                    {
                                        // Calculate footer height and opacity based on animation phase and direction
                                        let (footer_height_scale, footer_opacity) = if is_expanding {
                                            // Expanding: 0-20% height, 20-40% fade in, 40-100% visible
                                            if scale < 0.2 {
                                                // Phase 1 (0-20%): Height animation from 0% to 100%
                                                let phase_progress = scale / 0.2; // 0.0 to 1.0
                                                (phase_progress, 0.0)
                                            } else if scale < 0.4 {
                                                // Phase 2 (20-40%): Opacity animation from 0% to 100%
                                                let phase_progress = (scale - 0.2) / 0.2; // 0.0 to 1.0
                                                (1.0, phase_progress)
                                            } else {
                                                // Phase 3 (40-100%): Fully visible
                                                (1.0, 1.0)
                                            }
                                        } else {
                                            // Collapsing: 0-60% visible, 60-80% fade out, 80-100% height collapse
                                            if scale < 0.2 {
                                                // Phase 3 (0-20%): Height animation from 100% to 0%
                                                let phase_progress = scale / 0.2; // 0.0 to 1.0
                                                (phase_progress, 0.0)
                                            } else if scale < 0.4 {
                                                // Phase 2 (20-40%): Opacity animation from 100% to 0%
                                                let phase_progress = (scale - 0.2) / 0.2; // 0.0 to 1.0
                                                (1.0, phase_progress)
                                            } else {
                                                // Phase 1 (40-100%): Fully visible
                                                (1.0, 1.0)
                                            }
                                        };

                                        // Only show footer if height scale > 0
                                        if footer_height_scale > 0.0 {
                                            let (collapse_icon, collapse_text) = (
                                                file_icons::get().get_type_icon(file_icons::CHEVRON_UP),
                                                "Collapse",
                                            );
                                            elements.push(
                                                div()
                                                    .flex()
                                                    .justify_center()
                                                    .items_center()
                                                    .w_full()
                                                    .mt_1()
                                                    .border_t_1()
                                                    .border_color(cx.theme().border)
                                                    .cursor_pointer()
                                                    .hover(|s| s.bg(cx.theme().border.opacity(0.5)))
                                                    // Apply height scaling
                                                    .when(footer_height_scale < 1.0, |div| {
                                                        div.h(px(32.0 * footer_height_scale)) // Approximate footer height
                                                            .overflow_hidden()
                                                    })
                                                    .when(footer_height_scale >= 1.0, |div| {
                                                        div.p_1() // Normal padding when fully expanded
                                                    })
                                                    // Apply opacity
                                                    .opacity(footer_opacity)
                                                    .on_mouse_up(
                                                        MouseButton::Left,
                                                        cx.listener(move |view, _event, _window, cx| {
                                                            view.toggle_tool_collapsed(cx);
                                                        }),
                                                    )
                                                    .child(div().flex().items_center().gap_1().children(
                                                        vec![
                                                            file_icons::render_icon(
                                                                &collapse_icon,
                                                                14.0,
                                                                chevron_color,
                                                                "▲",
                                                            ).into_any(),
                                                            Label::new(collapse_text)
                                                                .text_color(chevron_color)
                                                                .into_any_element()
                                                        ],
                                                    ))
                                                    .into_any(),
                                            );
                                        }
                                    }
                                }

                                elements
                            }),
                        ),
                    ])
                    .shadow_sm()
                    .into_any_element()
            }
        }
    }
}

/// Regular text block
#[derive(Debug, Clone)]
pub struct TextBlock {
    pub content: String,
}

/// Thinking text block with collapsible content
#[derive(Debug, Clone)]
pub struct ThinkingBlock {
    pub content: String,
    pub is_collapsed: bool,
    pub is_completed: bool,
    pub start_time: std::time::Instant,
    pub end_time: std::time::Instant,
}

impl ThinkingBlock {
    pub fn new(content: String) -> Self {
        let now = std::time::Instant::now();
        Self {
            content,
            is_collapsed: true,  // Default is collapsed
            is_completed: false, // Default is not completed
            start_time: now,
            end_time: now, // Initially same as start_time
        }
    }

    pub fn formatted_duration(&self) -> String {
        // Calculate duration based on status
        let duration = if self.is_completed {
            // For completed blocks, use the stored end_time
            self.end_time.duration_since(self.start_time)
        } else {
            // For ongoing blocks, show elapsed time
            self.start_time.elapsed()
        };

        if duration.as_secs() < 60 {
            format!("{}s", duration.as_secs())
        } else {
            let minutes = duration.as_secs() / 60;
            let seconds = duration.as_secs() % 60;
            format!("{}m{}s", minutes, seconds)
        }
    }
}

/// Tool use block with name and parameters
#[derive(Debug, Clone)]
pub struct ToolUseBlock {
    pub name: String,
    pub id: String,
    pub parameters: Vec<ParameterBlock>,
    pub status: ToolStatus,
    pub status_message: Option<String>,
    pub output: Option<String>,
    pub is_collapsed: bool,
}

/// Parameter for a tool
#[derive(Debug, Clone)]
pub struct ParameterBlock {
    pub name: String,
    pub value: String,
}
