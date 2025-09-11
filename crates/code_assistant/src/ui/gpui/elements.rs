use crate::ui::gpui::file_icons;
use crate::ui::gpui::image;
use crate::ui::gpui::parameter_renderers::ParameterRendererRegistry;
use crate::ui::ToolStatus;
use gpui::{
    bounce, div, ease_in_out, img, percentage, px, svg, Animation, AnimationExt, Bounds, Context,
    Entity, ImageSource, IntoElement, MouseButton, ObjectFit, Pixels, SharedString, Styled, Task,
    Timer, Transformation,
};
use gpui::{prelude::*, FontWeight};
use gpui_component::{label::Label, ActiveTheme};

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::trace;

/// Maximum height for rendered images in pixels
const MAX_IMAGE_HEIGHT: f32 = 80.0;

/// Role of a message in the conversation
#[derive(Debug, Clone, PartialEq)]
pub enum MessageRole {
    User,
    Assistant,
}

/// State of a tool block for rendering and interaction
#[derive(Debug, Clone, PartialEq)]
pub enum ToolBlockState {
    /// Tool is collapsed - show parameters and output but collapsed
    Collapsed,
    /// Tool is expanded - show all content expanded
    Expanded,
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
    /// Current project for parameter filtering
    current_project: Arc<Mutex<String>>,
}

impl MessageContainer {
    pub fn with_role(role: MessageRole, _cx: &mut Context<Self>) -> Self {
        Self {
            elements: Arc::new(Mutex::new(Vec::new())),
            role,
            current_request_id: Arc::new(Mutex::new(0)),
            current_project: Arc::new(Mutex::new(String::new())),
        }
    }

    // Set the current request ID for this message container
    pub fn set_current_request_id(&self, request_id: u64) {
        *self.current_request_id.lock().unwrap() = request_id;
    }

    /// Set the current project for parameter filtering
    pub fn set_current_project(&self, project: String) {
        *self.current_project.lock().unwrap() = project;
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

        let request_id = *self.current_request_id.lock().unwrap();
        let mut elements = self.elements.lock().unwrap();
        let block = BlockData::TextBlock(TextBlock {
            content: content.into(),
        });
        let view = cx.new(|cx| BlockView::new(block, request_id, self.current_project.clone(), cx));
        elements.push(view);
        cx.notify();
    }

    // Add a new thinking block
    #[allow(dead_code)]
    pub fn add_thinking_block(&self, content: impl Into<String>, cx: &mut Context<Self>) {
        self.finish_any_thinking_blocks(cx);

        let request_id = *self.current_request_id.lock().unwrap();
        let mut elements = self.elements.lock().unwrap();
        let block = BlockData::ThinkingBlock(ThinkingBlock::new(content.into()));
        let view = cx.new(|cx| BlockView::new(block, request_id, self.current_project.clone(), cx));
        elements.push(view);
        cx.notify();
    }

    // Add a new image block
    pub fn add_image_block(
        &self,
        media_type: impl Into<String>,
        data: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.finish_any_thinking_blocks(cx);

        let media_type = media_type.into();
        let data = data.into();

        // Try to parse the base64 image data
        let image = image::parse_base64_image(&media_type, &data);

        let request_id = *self.current_request_id.lock().unwrap();
        let mut elements = self.elements.lock().unwrap();
        let block = BlockData::ImageBlock(ImageBlock { media_type, image });
        let view = cx.new(|cx| BlockView::new(block, request_id, self.current_project.clone(), cx));
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

        let request_id = *self.current_request_id.lock().unwrap();
        let mut elements = self.elements.lock().unwrap();
        let block = BlockData::ToolUse(ToolUseBlock {
            name: name.into(),
            id: id.into(),
            parameters: Vec::new(),
            status: ToolStatus::Pending,
            status_message: None,
            output: None,
            state: ToolBlockState::Collapsed, // Default to collapsed
        });
        let view = cx.new(|cx| BlockView::new(block, request_id, self.current_project.clone(), cx));
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
            let mut should_animate_collapse = false;
            let mut should_animate_expand = false;

            element.update(cx, |view, _cx| {
                if let Some(tool) = view.block.as_tool_mut() {
                    if tool.id == tool_id {
                        let was_generating = view.is_generating;
                        let had_streaming_output =
                            tool.output.as_ref().map(|o| !o.is_empty()).unwrap_or(false);

                        tool.status = status;
                        tool.status_message = message.clone();

                        // Set output if provided and we don't already have streaming output
                        if let Some(ref new_output) = output {
                            if !had_streaming_output {
                                tool.output = Some(new_output.clone());
                            }
                            // If we had streaming output, keep it (don't overwrite)
                        }

                        // Update generating state based on tool completion
                        if status == ToolStatus::Success || status == ToolStatus::Error {
                            if was_generating {
                                // Auto-collapse all tools after completion
                                // This keeps the UI clean regardless of streaming behavior
                                tool.state = ToolBlockState::Collapsed;
                                should_animate_collapse = true;
                            }
                            // If already not generating, no automatic state change
                        } else {
                            // For pending or in-progress status, ensure it's in generating state
                            if !view.is_generating {
                                // Tool is transitioning back to generating - trigger animation
                                should_animate_expand = true;
                            }
                            // If already generating, no need to animate
                        }

                        updated = true;
                    }
                }
            });

            // Handle generating state changes after the closure
            if should_animate_collapse || should_animate_expand {
                element.update(cx, |view, cx| {
                    if status == ToolStatus::Success || status == ToolStatus::Error {
                        view.set_generating(false);
                        if should_animate_collapse {
                            view.start_expand_collapse_animation(false, cx);
                        }
                    } else if should_animate_expand {
                        view.set_generating(true);
                        view.start_expand_collapse_animation(true, cx);
                    }
                });
            } else if updated && (status == ToolStatus::Success || status == ToolStatus::Error) {
                // Just update generating state without animation
                element.update(cx, |view, _cx| {
                    view.set_generating(false);
                });
            }

            // Handle animation in a separate update to avoid borrowing conflicts
            if should_animate_collapse || should_animate_expand {
                element.update(cx, |view, cx| {
                    if should_animate_collapse {
                        view.start_expand_collapse_animation(false, cx);
                    } else if should_animate_expand {
                        view.start_expand_collapse_animation(true, cx);
                    }
                });
            } else if updated {
                // If we updated but don't need animation, still notify for re-render
                element.update(cx, |_view, cx| {
                    cx.notify();
                });
            }
        }

        updated
    }

    // Add or append to text block
    pub fn add_or_append_to_text_block(&self, content: impl Into<String>, cx: &mut Context<Self>) {
        self.finish_any_thinking_blocks(cx);

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
        let view = cx.new(|cx| BlockView::new(block, request_id, self.current_project.clone(), cx));
        elements.push(view);
        cx.notify();
    }

    // Add or append to thinking block
    pub fn add_or_append_to_thinking_block(
        &self,
        content: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
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
        let view = cx.new(|cx| BlockView::new(block, request_id, self.current_project.clone(), cx));
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
                state: ToolBlockState::Collapsed, // Default to collapsed
            };

            tool.parameters.push(ParameterBlock {
                name: name.clone(),
                value: value.clone(),
            });

            let block = BlockData::ToolUse(tool);
            let view =
                cx.new(|cx| BlockView::new(block, request_id, self.current_project.clone(), cx));
            elements.push(view);
            cx.notify();
        }
    }

    // Mark a tool as ended (could add visual indicator)
    pub fn end_tool_use(&self, id: impl Into<String>, cx: &mut Context<Self>) {
        let id = id.into();
        let elements = self.elements.lock().unwrap();

        // Find the tool and mark it as completed
        for element in elements.iter() {
            cx.update_entity(element, |block_view, cx| {
                if let Some(tool_block) = block_view.block.as_tool_mut() {
                    if tool_block.id == id {
                        block_view.set_generating(false); // Mark as completed (not generating)
                        cx.notify(); // Trigger re-render to show virtual parameters
                    }
                }
            }); // Ignore errors from update_entity
        }
    }

    // Append streaming output to a tool block
    pub fn append_tool_output(
        &self,
        tool_id: impl Into<String>,
        chunk: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let tool_id = tool_id.into();
        let chunk = chunk.into();
        let elements = self.elements.lock().unwrap();

        // Find the tool and append the output chunk
        for element in elements.iter() {
            cx.update_entity(element, |block_view, cx| {
                if let Some(tool_block) = block_view.block.as_tool_mut() {
                    if tool_block.id == tool_id {
                        // Append to existing output or create new output
                        if let Some(existing_output) = &mut tool_block.output {
                            existing_output.push_str(&chunk);
                        } else {
                            tool_block.output = Some(chunk.clone());
                        }
                        cx.notify(); // Trigger re-render
                    }
                }
            }); // Ignore errors from update_entity
        }
    }

    fn finish_any_thinking_blocks(&self, cx: &mut Context<Self>) {
        let elements = self.elements.lock().unwrap();

        // Mark any previous thinking blocks as completed and not generating
        for element in elements.iter() {
            element.update(cx, |view, cx| {
                if let Some(thinking_block) = view.block.as_thinking_mut() {
                    if !thinking_block.is_completed {
                        thinking_block.is_completed = true;
                        thinking_block.end_time = std::time::Instant::now();
                        view.set_generating(false);
                        cx.notify();
                    }
                }
            });
        }
    }

    /// Update reasoning summary for the most recent thinking block
    pub fn update_reasoning_summary(&self, id: String, delta: String, cx: &mut Context<Self>) {
        let mut elements = self.elements.lock().unwrap();

        if let Some(last) = elements.last() {
            let mut was_updated = false;

            last.update(cx, |view, cx| {
                if let Some(thinking_block) = view.block.as_thinking_mut() {
                    thinking_block.update_reasoning_summary(id.clone(), delta.clone());
                    was_updated = true;
                    cx.notify();
                }
            });

            if was_updated {
                return;
            }
        }

        // If we reach here, we need to add a new thinking block
        let request_id = *self.current_request_id.lock().unwrap();
        let mut new_thinking_block = ThinkingBlock::new(String::new());
        new_thinking_block.update_reasoning_summary(id, delta);

        let block = BlockData::ThinkingBlock(new_thinking_block);
        let view = cx.new(|cx| BlockView::new(block, request_id, self.current_project.clone(), cx));
        elements.push(view);
        cx.notify();
    }

    /// Complete reasoning for the most recent thinking block
    pub fn complete_reasoning(&self, cx: &mut Context<Self>) {
        let elements = self.elements.lock().unwrap();
        if let Some(last) = elements.last() {
            last.update(cx, |view, cx| {
                if let Some(thinking_block) = view.block.as_thinking_mut() {
                    thinking_block.complete_reasoning();
                    view.set_generating(false);
                    cx.notify();
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
    ImageBlock(ImageBlock),
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
    is_generating: bool, // Universal generating state for all block types
    // Animation state
    animation_state: AnimationState,
    content_height: Rc<Cell<Pixels>>,
    animation_task: Option<Task<()>>,
    // Current project for parameter filtering
    current_project: Arc<Mutex<String>>,
}

impl BlockView {
    pub fn new(
        block: BlockData,
        request_id: u64,
        current_project: Arc<Mutex<String>>,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            block,
            request_id,
            is_generating: true, // Default to generating when first created
            animation_state: AnimationState::Idle,
            content_height: Rc::new(Cell::new(px(0.0))),
            animation_task: None,
            current_project,
        }
    }

    /// Check if this block is an image block
    pub fn is_image_block(&self) -> bool {
        matches!(self.block, BlockData::ImageBlock(_))
    }

    /// Set the generating state of this block
    pub fn set_generating(&mut self, generating: bool) {
        self.is_generating = generating;
    }

    /// Check if this block can toggle expansion (some blocks can't while generating)
    pub fn can_toggle_expansion(&self) -> bool {
        match &self.block {
            BlockData::ToolUse(_) => !self.is_generating, // Tools can't toggle while generating
            BlockData::ThinkingBlock(_) => true,          // Thinking blocks can always toggle
            _ => false,                                   // Other blocks don't have expansion
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
        // Check if we can toggle expansion
        if !self.can_toggle_expansion() {
            return;
        }

        let should_expand = if let Some(tool) = self.block.as_tool_mut() {
            match tool.state {
                ToolBlockState::Collapsed => {
                    // Toggle to expanded
                    tool.state = ToolBlockState::Expanded;
                    true
                }
                ToolBlockState::Expanded => {
                    // Toggle to collapsed
                    tool.state = ToolBlockState::Collapsed;
                    false
                }
            }
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

                // Define header text based on state using reasoning-aware method
                let header_text = block.get_display_title(self.is_generating);

                // Use theme utilities for colors
                let blue_base = cx.theme().info; // Theme color for thinking block
                let thinking_bg = crate::ui::gpui::theme::colors::thinking_block_bg(cx.theme());
                let chevron_color =
                    crate::ui::gpui::theme::colors::thinking_block_chevron(cx.theme());
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
                                            // Expanded view - use reasoning-aware content
                                            let content =
                                                block.get_expanded_content(self.is_generating);
                                            div()
                                                .pt_1()
                                                .text_size(px(14.))
                                                .italic()
                                                .text_color(text_color)
                                                .child(gpui_component::text::TextView::markdown(
                                                    "thinking-content",
                                                    content,
                                                ))
                                                .into_any()
                                        } else {
                                            // If collapsed, show a preview of the first line
                                            let content =
                                                block.get_expanded_content(self.is_generating);
                                            let first_line =
                                                content.lines().next().unwrap_or("").to_string();
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

                // Get the chevron icon based on block state
                let (chevron_icon, chevron_text) = if self.is_generating {
                    // Show disabled/different icon while generating
                    (file_icons::get().get_type_icon(file_icons::CHEVRON_UP), "▲")
                } else {
                    match block.state {
                        ToolBlockState::Collapsed => (
                            file_icons::get().get_type_icon(file_icons::CHEVRON_DOWN),
                            "▼",
                        ),
                        ToolBlockState::Expanded => {
                            (file_icons::get().get_type_icon(file_icons::CHEVRON_UP), "▲")
                        }
                    }
                };

                // Use theme utilities for colors
                let icon_color =
                    crate::ui::gpui::theme::colors::tool_block_icon(cx.theme(), &block.status);
                let tool_name_color =
                    crate::ui::gpui::theme::colors::tool_block_name(cx.theme(), &block.status);
                let status_color = crate::ui::gpui::theme::colors::tool_border_by_status(
                    cx.theme(),
                    &block.status,
                );
                let tool_bg = crate::ui::gpui::theme::colors::tool_block_bg(cx.theme());
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
                            cx.theme(),
                        )
                    } else {
                        // Fallback to empty element
                        div().into_any_element()
                    }
                };

                // Filter out hidden parameters, then separate into regular and full-width
                let current_project = self.current_project.lock().unwrap().clone();
                let registry = ParameterRendererRegistry::global();

                // Convert parameters to HashMap for virtual parameter processing
                let param_map: std::collections::HashMap<String, String> = block
                    .parameters
                    .iter()
                    .map(|p| (p.name.clone(), p.value.clone()))
                    .collect();

                let should_hide_param = |param: &ParameterBlock| {
                    // Hide project parameter logic (existing)
                    let hide_project = param.name == "project"
                        && !current_project.is_empty()
                        && param.value == current_project;

                    // Hide parameters that are part of virtual parameters (NEW)
                    let hide_virtual = registry.as_ref().is_some_and(|reg| {
                        reg.should_hide_parameter(
                            &block.name,
                            &param.name,
                            &param_map,
                            !self.is_generating, // completed = not generating
                        )
                    });

                    hide_project || hide_virtual
                };

                let visible_params: Vec<&ParameterBlock> = block
                    .parameters
                    .iter()
                    .filter(|param| !should_hide_param(param))
                    .collect();

                // Get virtual parameters that should be rendered
                let virtual_elements: Vec<gpui::AnyElement> = if let Some(registry) = &registry {
                    registry.render_virtual_parameters(
                        &block.name,
                        &param_map,
                        !self.is_generating, // completed = not generating
                        cx.theme(),
                    )
                } else {
                    Vec::new()
                };
                let has_virtual_elements = !virtual_elements.is_empty();

                let (regular_params, fullwidth_params): (
                    Vec<&ParameterBlock>,
                    Vec<&ParameterBlock>,
                ) = visible_params.into_iter().partition(|param| {
                    !registry.as_ref().is_some_and(|reg| {
                        reg.get_renderer(&block.name, &param.name)
                            .is_full_width(&block.name, &param.name)
                    })
                });

                div()
                    .rounded(px(4.))
                    .my_2()
                    .bg(tool_bg)
                    .shadow_xs()
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
                            .child(div().w(px(8.)).h_full().rounded(px(4.)).bg(status_color)),
                        div().flex_grow().min_w_0().relative().child(
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
                                                .hover(|s| s.bg(status_color.opacity(0.2)))
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
                                        AnimationState::Idle => {
                                            if self.is_generating {
                                                1.0 // Show content while generating
                                            } else {
                                                match block.state {
                                                    ToolBlockState::Collapsed => 0.0,  // Hide content when collapsed
                                                    ToolBlockState::Expanded => 1.0,   // Show content when expanded
                                                }
                                            }
                                        }
                                    };

                                    let content_height_rc = self.content_height.clone();
                                    let has_expandable_content = !fullwidth_params.is_empty() ||
                                        has_virtual_elements ||
                                        block.output.as_ref().is_some_and(|o| !o.is_empty());

                                    if has_expandable_content && (self.is_generating || block.state != ToolBlockState::Collapsed || scale > 0.0) {
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

                                                            // Virtual parameters
                                                            if has_virtual_elements {
                                                                expandable_elements.push(
                                                                    div()
                                                                        .flex()
                                                                        .flex_col()
                                                                        .w_full()
                                                                        .mt_1()
                                                                        .children(virtual_elements)
                                                                        .into_any(),
                                                                );
                                                            }

                                                            // Smart output rendering based on streaming vs batch
                                                            if let Some(output_content) = &block.output {
                                                                if !output_content.is_empty() {
                                                                    let should_show_output = if self.is_generating {
                                                                        // While generating: show output only if we have any
                                                                        // (This means we received ToolOutput fragments)
                                                                        true
                                                                    } else {
                                                                        // After completion: show output only when expanded
                                                                        // (User can manually expand to see batch output)
                                                                        block.state == ToolBlockState::Expanded
                                                                    };

                                                                    if should_show_output {
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
                                                            }

                                                            expandable_elements
                                                        })
                                                        // Add bottom padding to prevent content overlapping with collapse bar
                                                        .when(
                                                            !fullwidth_params.is_empty() ||
                                                            has_virtual_elements ||
                                                            (!self.is_generating &&
                                                             block.output.as_ref().is_some_and(|o| !o.is_empty())),
                                                            |div| div.pb(px(24.0))
                                                        )
                                                )
                                                .into_any(),
                                        );
                                    }
                                }

                                // Error message (only shown for error status when collapsed, or when there's no output)
                                if block.status == crate::ui::ToolStatus::Error
                                    && block.status_message.is_some()
                                    && ((!self.is_generating && block.state == ToolBlockState::Collapsed)
                                        || block.output.as_ref().is_none_or(|o| o.is_empty()))
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

                                elements
                            }),
                        ).child({
                            // Absolutely positioned collapse bar on top of the content
                            let (scale, is_expanding) = match &self.animation_state {
                                AnimationState::Animating { height_scale, target, .. } => (*height_scale, *target == 1.0),
                                AnimationState::Idle => {
                                    if self.is_generating {
                                        (1.0, false)
                                    } else {
                                        match block.state {
                                            ToolBlockState::Collapsed => (0.0, false),
                                            ToolBlockState::Expanded => (1.0, false),
                                        }
                                    }
                                }
                            };

                            let has_expandable_content = !fullwidth_params.is_empty() ||
                                has_virtual_elements ||
                                (!self.is_generating &&
                                 block.output.as_ref().is_some_and(|o| !o.is_empty()));

                            if has_expandable_content && (self.is_generating || block.state != ToolBlockState::Collapsed || scale > 0.0) {
                                // Calculate opacity based on animation phase and direction
                                let footer_opacity = match &self.animation_state {
                                    AnimationState::Idle => {
                                        if self.is_generating || block.state == ToolBlockState::Expanded {
                                            1.0
                                        } else {
                                            0.0 // Collapsed
                                        }
                                    },
                                    AnimationState::Animating { .. } => {
                                        if is_expanding {
                                            // Expanding: fade in during first 30% of animation
                                            if scale < 0.3 {
                                                scale / 0.3
                                            } else {
                                                1.0
                                            }
                                        } else {
                                            // Collapsing: fade out during last 30% of animation
                                            if scale > 0.7 {
                                                (scale - 0.7) / 0.3
                                            } else {
                                                0.0
                                            }
                                        }
                                    }
                                };

                                let (collapse_icon, collapse_text) = (
                                    file_icons::get().get_type_icon(file_icons::CHEVRON_UP),
                                    "Collapse",
                                );

                                div()
                                    .absolute()
                                    .bottom_0()
                                    .left_0()
                                    .right_0()
                                    .flex()
                                    .justify_center()
                                    .items_center()
                                    .text_xs()
                                    .h(px(24.0))
                                    .border_t_1()
                                    .border_color(cx.theme().border)
                                    .bg(tool_bg)
                                    .cursor_pointer()
                                    .hover(|s| s.bg(cx.theme().border.opacity(0.5)))
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
                                    .into_any()
                            } else {
                                div().into_any()
                            }
                        }),
                    ])
                    .into_any_element()
            }
            BlockData::ImageBlock(block) => {
                if let Some(image) = &block.image {
                    // Render the actual image - margins/spacing handled by parent container
                    div()
                        .flex_none() // Don't grow or shrink
                        .child(
                            div()
                                .border_1()
                                .border_color(cx.theme().border)
                                .rounded_md()
                                .overflow_hidden()
                                .bg(cx.theme().card)
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
    // NEW: OpenAI reasoning fields
    pub reasoning_summary_items: Vec<llm::ReasoningSummaryItem>,
    pub current_generating_title: Option<String>,
    pub current_generating_content: Option<String>,
    pub current_reasoning_item_id: Option<String>,
}

/// Image block with media type and base64 data
#[derive(Debug, Clone)]
pub struct ImageBlock {
    pub media_type: String,
    /// Parsed image ready for rendering, if parsing was successful
    pub image: Option<Arc<gpui::Image>>,
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
            // Initialize reasoning fields
            reasoning_summary_items: Vec::new(),
            current_generating_title: None,
            current_generating_content: None,
            current_reasoning_item_id: None,
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
            format!("{minutes}m{seconds}s")
        }
    }

    /// Update reasoning summary with new content
    pub fn update_reasoning_summary(&mut self, id: String, delta: String) {
        // Check if this is a new item
        if self.current_reasoning_item_id.as_ref() != Some(&id) {
            // If we had a previous item, finalize it
            if let Some(_prev_id) = &self.current_reasoning_item_id {
                if let Some(content) = &self.current_generating_content {
                    self.reasoning_summary_items
                        .push(llm::ReasoningSummaryItem::SummaryText {
                            text: content.clone(),
                        });
                }
            }

            // Start tracking the new item
            self.current_reasoning_item_id = Some(id);
            self.current_generating_content = Some(String::new());
            self.current_generating_title = None;
        }

        // Append delta to current content
        if let Some(content) = &mut self.current_generating_content {
            content.push_str(&delta);

            // Try to parse title if we don't have one yet
            if self.current_generating_title.is_none() {
                self.current_generating_title = Self::parse_title_from_content(content);
            }
        }
    }

    /// Complete reasoning and finalize any remaining items
    pub fn complete_reasoning(&mut self) {
        use tracing::debug;

        debug!(
            "ThinkingBlock::complete_reasoning called - current_id: {:?}, current_content: {:?}, existing_items: {}",
            self.current_reasoning_item_id,
            self.current_generating_content.as_ref().map(|c| c.len()),
            self.reasoning_summary_items.len()
        );

        // Finalize current item if any
        if let Some(_current_id) = &self.current_reasoning_item_id {
            if let Some(content) = &self.current_generating_content {
                self.reasoning_summary_items
                    .push(llm::ReasoningSummaryItem::SummaryText {
                        text: content.clone(),
                    });
                debug!(
                    "Added final reasoning item, total items: {}",
                    self.reasoning_summary_items.len()
                );
            }

            // Clear current state
            self.current_reasoning_item_id = None;
            self.current_generating_title = None;
            self.current_generating_content = None;
        }

        // Ensure we have content to display - if we have reasoning items but no fallback content,
        // populate the fallback content with the joined reasoning content
        if !self.reasoning_summary_items.is_empty() && self.content.is_empty() {
            self.content = self
                .reasoning_summary_items
                .iter()
                .map(|item| match item {
                    llm::ReasoningSummaryItem::SummaryText { text } => text.clone(),
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            debug!(
                "Populated fallback content with reasoning items, content_len: {}",
                self.content.len()
            );
        }

        debug!(
            "ThinkingBlock::complete_reasoning finished - total items: {}, content_len: {}",
            self.reasoning_summary_items.len(),
            self.content.len()
        );
    }

    /// Get display title based on generating state
    pub fn get_display_title(&self, is_generating: bool) -> String {
        if is_generating {
            // While generating, show current summary title or "Thinking..."
            self.current_generating_title
                .as_deref()
                .unwrap_or("Thinking...")
                .to_string()
        } else {
            // When completed, show duration
            format!("Thought for {}", self.formatted_duration())
        }
    }

    /// Get expanded content based on generating state
    pub fn get_expanded_content(&self, is_generating: bool) -> String {
        use tracing::debug;

        let result = if is_generating {
            // While generating, show current item content
            let content = self
                .current_generating_content
                .as_deref()
                .unwrap_or(&self.content)
                .to_string();
            debug!(
                "get_expanded_content(generating=true): current_content_len={}, fallback_content_len={}",
                self.current_generating_content.as_ref().map(|c| c.len()).unwrap_or(0),
                self.content.len()
            );
            content
        } else if self.is_reasoning_block() {
            // When completed with reasoning, show all summary items as raw content
            let reasoning_content = self
                .reasoning_summary_items
                .iter()
                .map(|item| match item {
                    llm::ReasoningSummaryItem::SummaryText { text } => text.clone(),
                })
                .collect::<Vec<_>>()
                .join("\n\n");

            debug!(
                "get_expanded_content(generating=false, is_reasoning=true): items={}, reasoning_content_len={}, fallback_content_len={}",
                self.reasoning_summary_items.len(),
                reasoning_content.len(),
                self.content.len()
            );

            // Fallback: if reasoning_summary_items is empty but we had content,
            // there might have been a timing issue during completion
            if reasoning_content.is_empty() && !self.content.is_empty() {
                self.content.clone()
            } else {
                reasoning_content
            }
        } else {
            // Traditional thinking block
            debug!(
                "get_expanded_content(generating=false, is_reasoning=false): content_len={}",
                self.content.len()
            );
            self.content.clone()
        };

        debug!("get_expanded_content final result_len={}", result.len());
        result
    }

    /// Check if this is a reasoning block (has reasoning summary items)
    pub fn is_reasoning_block(&self) -> bool {
        !self.reasoning_summary_items.is_empty()
            || self.current_reasoning_item_id.is_some()
            || self.current_generating_content.is_some()
    }

    /// Parse title from reasoning content in OpenAI format "**title**\n\ncontent"
    fn parse_title_from_content(content: &str) -> Option<String> {
        // Look for markdown bold pattern: **title** followed by newlines
        if let Some(start) = content.find("**") {
            if let Some(end) = content[start + 2..].find("**") {
                let title_end = start + 2 + end;
                let title = content[start + 2..title_end].trim();
                if !title.is_empty() {
                    return Some(title.to_string());
                }
            }
        }

        // Fallback: use the first line or first few words
        let first_line = content.lines().next().unwrap_or(content);
        let words: Vec<&str> = first_line.split_whitespace().take(5).collect();
        if !words.is_empty() {
            Some(words.join(" "))
        } else {
            None
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
    pub state: ToolBlockState, // Only collapsed/expanded, no generating
}

/// Parameter for a tool
#[derive(Debug, Clone)]
pub struct ParameterBlock {
    pub name: String,
    pub value: String,
}
