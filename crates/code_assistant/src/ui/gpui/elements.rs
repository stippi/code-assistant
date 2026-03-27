use crate::persistence::{BranchInfo, NodeId};
use crate::ui::gpui::file_icons;
use crate::ui::gpui::image;

use crate::ui::ToolStatus;
use gpui::{
    div, img, percentage, px, svg, Animation, AnimationExt, ClickEvent, Context, Entity,
    ImageSource, IntoElement, ObjectFit, Pixels, SharedString, Styled, Task, Timer, Transformation,
};
use gpui::{prelude::*, FontWeight};
use gpui_component::{text::TextView, ActiveTheme};

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

    /// Current project for parameter filtering (used to detect cross-project tool calls)
    #[allow(dead_code)]
    current_project: Arc<Mutex<String>>,
    /// Tracks the last block type for hidden tool paragraph breaks
    last_block_type_for_hidden_tool: Arc<Mutex<Option<HiddenToolBlockType>>>,
    /// Flag indicating a hidden tool completed and we may need a paragraph break
    needs_paragraph_break_after_hidden_tool: Arc<Mutex<bool>>,

    /// Node ID for this message (for branching support)
    node_id: Arc<Mutex<Option<NodeId>>>,
    /// Branch info if this message is part of a branch point
    branch_info: Arc<Mutex<Option<BranchInfo>>>,
}

/// Tracks the last block type for paragraph breaks after hidden tools
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HiddenToolBlockType {
    Text,
    Thinking,
}

impl MessageContainer {
    pub fn with_role(role: MessageRole, _cx: &mut Context<Self>) -> Self {
        Self {
            elements: Arc::new(Mutex::new(Vec::new())),
            role,

            current_request_id: Arc::new(Mutex::new(0)),
            current_project: Arc::new(Mutex::new(String::new())),
            last_block_type_for_hidden_tool: Arc::new(Mutex::new(None)),
            needs_paragraph_break_after_hidden_tool: Arc::new(Mutex::new(false)),
            node_id: Arc::new(Mutex::new(None)),
            branch_info: Arc::new(Mutex::new(None)),
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

    /// Set the node ID for this message (for branching support)
    pub fn set_node_id(&self, node_id: Option<NodeId>) {
        *self.node_id.lock().unwrap() = node_id;
    }

    /// Get the node ID for this message
    pub fn node_id(&self) -> Option<NodeId> {
        *self.node_id.lock().unwrap()
    }

    /// Set the branch info for this message
    pub fn set_branch_info(&self, branch_info: Option<BranchInfo>) {
        *self.branch_info.lock().unwrap() = branch_info;
    }

    /// Get the branch info for this message
    pub fn branch_info(&self) -> Option<BranchInfo> {
        self.branch_info.lock().unwrap().clone()
    }

    /// Mark that a hidden tool completed - paragraph break may be needed before next text
    pub fn mark_hidden_tool_completed(&self, _cx: &mut Context<Self>) {
        *self.needs_paragraph_break_after_hidden_tool.lock().unwrap() = true;
    }

    /// Check if we need a paragraph break after a hidden tool and return it if so
    fn get_paragraph_break_if_needed(
        &self,
        current_block_type: HiddenToolBlockType,
    ) -> Option<String> {
        let mut needs_break = self.needs_paragraph_break_after_hidden_tool.lock().unwrap();
        if !*needs_break {
            return None;
        }

        // Reset the flag
        *needs_break = false;

        // Check if the block type matches the last one
        let last_type = *self.last_block_type_for_hidden_tool.lock().unwrap();
        if last_type == Some(current_block_type) {
            // Same type as before the hidden tool - need paragraph break
            Some("\n\n".to_string())
        } else {
            None
        }
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

    pub fn add_compaction_divider(&self, summary: impl Into<String>, cx: &mut Context<Self>) {
        self.finish_any_thinking_blocks(cx);

        let request_id = *self.current_request_id.lock().unwrap();
        let mut elements = self.elements.lock().unwrap();
        let block = BlockData::CompactionSummary(CompactionSummaryBlock {
            summary: summary.into(),
            is_expanded: false,
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
        let name = name.into();

        // Card-style tools start expanded; inline/other tools start collapsed
        let initial_state = {
            let is_card =
                crate::ui::gpui::tool_block_renderers::ToolBlockRendererRegistry::global()
                    .as_ref()
                    .and_then(|registry| registry.get(&name).cloned())
                    .is_some_and(|r| {
                        r.style() == crate::ui::gpui::tool_block_renderers::ToolBlockStyle::Card
                    });
            if is_card {
                ToolBlockState::Expanded
            } else {
                ToolBlockState::Collapsed
            }
        };

        let block = BlockData::ToolUse(ToolUseBlock {
            name,
            id: id.into(),
            parameters: Vec::new(),
            status: ToolStatus::Pending,
            status_message: None,
            output: None,
            state: initial_state,
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
            element.update(cx, |view, cx| {
                if let Some(tool) = view.block.as_tool_mut() {
                    if tool.id == tool_id {
                        tool.status = status;
                        tool.status_message = message.clone();

                        // Update output if provided
                        // Note: UpdateToolStatus always replaces output (used by spawn_agent for JSON updates)
                        // AppendToolOutput is used for streaming append behavior
                        if let Some(ref new_output) = output {
                            tool.output = Some(new_output.clone());
                        }

                        // Update generating flag on completion — no automatic state changes.
                        // The tool's collapse/expand state stays exactly as it was set at
                        // creation time (Card=Expanded, Inline=Collapsed) or as toggled
                        // by the user. The user is always in control.
                        if status == ToolStatus::Success || status == ToolStatus::Error {
                            view.set_generating(false);
                        } else if !view.is_generating {
                            view.set_generating(true);
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

        // Check if we need to insert a paragraph break after a hidden tool
        let paragraph_prefix = self.get_paragraph_break_if_needed(HiddenToolBlockType::Text);

        // Track block type for future hidden tool events
        *self.last_block_type_for_hidden_tool.lock().unwrap() = Some(HiddenToolBlockType::Text);

        let content = content.into();
        let mut elements = self.elements.lock().unwrap();

        if let Some(last) = elements.last() {
            let mut was_appended = false;

            last.update(cx, |view, cx| {
                if let Some(text_block) = view.block.as_text_mut() {
                    if let Some(prefix) = &paragraph_prefix {
                        text_block.content.push_str(prefix);
                    }
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
        let final_content = if let Some(prefix) = paragraph_prefix {
            format!("{}{}", prefix, content)
        } else {
            content.to_string()
        };
        let block = BlockData::TextBlock(TextBlock {
            content: final_content,
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
        // Check if we need to insert a paragraph break after a hidden tool
        let paragraph_prefix = self.get_paragraph_break_if_needed(HiddenToolBlockType::Thinking);

        // Track block type for future hidden tool events
        *self.last_block_type_for_hidden_tool.lock().unwrap() = Some(HiddenToolBlockType::Thinking);

        let content = content.into();
        let mut elements = self.elements.lock().unwrap();

        if let Some(last) = elements.last() {
            let mut was_appended = false;

            last.update(cx, |view, cx| {
                if let Some(thinking_block) = view.block.as_thinking_mut() {
                    if let Some(prefix) = &paragraph_prefix {
                        thinking_block.content.push_str(prefix);
                    }
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
        let final_content = if let Some(prefix) = paragraph_prefix {
            format!("{}{}", prefix, content)
        } else {
            content.to_string()
        };
        let block = BlockData::ThinkingBlock(ThinkingBlock::new(final_content));
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

    /// Replace a tool parameter value entirely (used by post-execution updates
    /// like format-on-save, where the tool modified its own input).
    pub fn replace_tool_parameter(
        &self,
        tool_id: impl Into<String>,
        name: impl Into<String>,
        value: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let tool_id = tool_id.into();
        let name = name.into();
        let value = value.into();
        let elements = self.elements.lock().unwrap();

        for element in elements.iter().rev() {
            let mut found = false;
            element.update(cx, |view, cx| {
                if let Some(tool) = view.block.as_tool_mut() {
                    if tool.id == tool_id {
                        for param in tool.parameters.iter_mut() {
                            if param.name == name {
                                param.value = value.clone();
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            // Parameter doesn't exist yet — add it
                            tool.parameters.push(ParameterBlock {
                                name: name.clone(),
                                value: value.clone(),
                            });
                            found = true;
                        }
                        cx.notify();
                    }
                }
            });
            if found {
                return;
            }
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
                        // Finalize any reasoning content before marking as completed
                        thinking_block.complete_reasoning();

                        thinking_block.is_completed = true;
                        thinking_block.end_time = std::time::Instant::now();
                        view.set_generating(false);

                        cx.notify();
                    }
                }
            });
        }
    }

    /// Start a new reasoning summary item for the most recent thinking block
    pub fn start_reasoning_summary_item(&self, cx: &mut Context<Self>) {
        let mut elements = self.elements.lock().unwrap();

        if let Some(last) = elements.last() {
            last.update(cx, |view, cx| {
                if let Some(thinking_block) = view.block.as_thinking_mut() {
                    thinking_block.start_reasoning_summary_item();
                    cx.notify();
                }
            });
            return;
        }

        // If we reach here, we need to add a new thinking block
        let request_id = *self.current_request_id.lock().unwrap();
        let mut new_thinking_block = ThinkingBlock::new(String::new());
        new_thinking_block.start_reasoning_summary_item();

        let block = BlockData::ThinkingBlock(new_thinking_block);
        let view = cx.new(|cx| BlockView::new(block, request_id, self.current_project.clone(), cx));
        elements.push(view);
        cx.notify();
    }

    /// Append delta content to the current reasoning summary item
    pub fn append_reasoning_summary_delta(&self, delta: String, cx: &mut Context<Self>) {
        let mut elements = self.elements.lock().unwrap();

        if let Some(last) = elements.last() {
            let mut was_updated = false;

            last.update(cx, |view, cx| {
                if let Some(thinking_block) = view.block.as_thinking_mut() {
                    thinking_block.append_reasoning_summary_delta(delta.clone());
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
        new_thinking_block.start_reasoning_summary_item();
        new_thinking_block.append_reasoning_summary_delta(delta);

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
    CompactionSummary(CompactionSummaryBlock),
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

    fn as_compaction_mut(&mut self) -> Option<&mut CompactionSummaryBlock> {
        match self {
            BlockData::CompactionSummary(b) => Some(b),
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
    /// Current project for parameter filtering (used to detect cross-project tool calls)
    #[allow(dead_code)]
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

    fn toggle_compaction(&mut self, cx: &mut Context<Self>) {
        if let Some(summary) = self.block.as_compaction_mut() {
            summary.is_expanded = !summary.is_expanded;
            cx.notify();
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

    // ------------------------------------------------------------------
    // Card skeleton (shown while parameters are still streaming)
    // ------------------------------------------------------------------

    /// Render a minimal card header for a tool whose renderer returned `None`
    /// (typically because parameters haven't arrived yet). This prevents the
    /// ugly `[edit]` / `[spawn_agent]` text flash.
    fn render_card_skeleton(
        &self,
        block: &ToolUseBlock,
        renderer: &dyn crate::ui::gpui::tool_block_renderers::ToolBlockRenderer,
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
                            .text_size(px(12.0))
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
        renderer: &dyn crate::ui::gpui::tool_block_renderers::ToolBlockRenderer,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        use crate::ui::gpui::theme::colors;

        let theme = cx.theme().clone();

        // Icon
        let icon = file_icons::get().get_tool_icon(&block.name);
        let (icon_color, desc_color) = match block.status {
            ToolStatus::Error => (theme.danger, theme.danger),
            ToolStatus::Running | ToolStatus::Pending => {
                if self.is_generating {
                    (theme.muted_foreground, theme.muted_foreground)
                } else {
                    (
                        colors::tool_block_icon(&theme, &block.status),
                        theme.foreground,
                    )
                }
            }
            ToolStatus::Success => (theme.muted_foreground, theme.muted_foreground),
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
        let has_output = block.output.as_ref().is_some_and(|o| !o.is_empty());
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
        let mut container = div().w_full();

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
                            .text_size(px(13.))
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
                container =
                    container.child(crate::ui::gpui::tool_block_renderers::animated_card_body(
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
        match &self.block {
            BlockData::TextBlock(block) => {
                // Use TextView with Markdown for rendering text
                div()
                    .text_color(cx.theme().foreground)
                    .child(
                        TextView::markdown("md-block", block.content.clone(), window, cx)
                            .selectable(true),
                    )
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
                                    .text_size(px(14.))
                                    .italic()
                                    .text_color(text_color)
                                    .child(TextView::markdown(
                                        "thinking-content",
                                        content,
                                        window,
                                        cx,
                                    ))
                                    .into_any()
                            } else {
                                div().into_any()
                            };

                            crate::ui::gpui::tool_block_renderers::animated_card_body(
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
                    crate::ui::gpui::tool_block_renderers::ToolBlockRendererRegistry::global()
                {
                    if let Some(renderer) = registry.get(&block.name) {
                        match renderer.style() {
                            crate::ui::gpui::tool_block_renderers::ToolBlockStyle::Inline => {
                                let block_clone = block.clone();
                                return self
                                    .render_inline_tool(&block_clone, renderer.as_ref(), window, cx)
                                    .into_any_element();
                            }

                            crate::ui::gpui::tool_block_renderers::ToolBlockStyle::Card => {
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
                                let card_ctx =
                                    crate::ui::gpui::tool_block_renderers::CardRenderContext {
                                        animation_scale: scale,
                                        is_collapsed: block.state == ToolBlockState::Collapsed,
                                        content_height: self.content_height.clone(),
                                    };

                                if let Some(element) = renderer.render(
                                    &block_clone,
                                    self.is_generating,
                                    &theme,
                                    Some(&card_ctx),
                                    window,
                                    cx,
                                ) {
                                    return element;
                                }
                                // Renderer returned None (e.g. parameters still
                                // streaming) — show a skeleton card with just
                                // the header so we don't flash a raw "[name]"
                                // placeholder.
                                return self.render_card_skeleton(block, renderer.as_ref(), &theme);
                            }
                        }
                    } else {
                        tracing::warn!("No ToolBlockRenderer registered for tool '{}'", block.name);
                    }
                }
                div()
                    .px_2()
                    .py_1()
                    .text_color(cx.theme().muted_foreground)
                    .text_size(px(13.))
                    .child(format!("[{}]", block.name))
                    .into_any_element()
            }
            BlockData::CompactionSummary(block) => {
                let icon = file_icons::get().get_type_icon(file_icons::MESSAGE_BUBBLES);
                let icon_color = cx.theme().info;
                let toggle_label = if block.is_expanded {
                    "Hide summary"
                } else {
                    "Show summary"
                };

                let header = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .children(vec![
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .children(vec![
                                file_icons::render_icon_container(&icon, 18.0, icon_color, "ℹ️")
                                    .into_any_element(),
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight(600.0))
                                    .text_color(icon_color)
                                    .child("Conversation compacted")
                                    .into_any_element(),
                            ])
                            .into_any_element(),
                        div()
                            .id("compaction-toggle")
                            .text_sm()
                            .text_color(cx.theme().link)
                            .cursor_pointer()
                            .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                                view.toggle_compaction(cx);
                            }))
                            .child(toggle_label)
                            .into_any_element(),
                    ])
                    .into_any_element();

                let mut children = vec![header];

                if block.is_expanded {
                    children.push(
                        div()
                            .text_color(cx.theme().foreground)
                            .child(
                                TextView::markdown(
                                    "compaction-summary",
                                    block.summary.clone(),
                                    window,
                                    cx,
                                )
                                .selectable(true),
                            )
                            .into_any_element(),
                    );
                } else {
                    let preview_text = block.summary.trim();
                    if !preview_text.is_empty() {
                        let first_line = preview_text.lines().next().unwrap_or("");
                        let truncated = if first_line.len() > 120 {
                            format!("{}…", &first_line[..120])
                        } else {
                            first_line.to_string()
                        };
                        children.push(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(truncated)
                                .into_any_element(),
                        );
                    }
                }

                div()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().popover)
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(children)
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

#[derive(Debug, Clone)]
pub struct CompactionSummaryBlock {
    pub summary: String,
    pub is_expanded: bool,
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

    /// Start a new reasoning summary item, finalizing the previous one if present
    pub fn start_reasoning_summary_item(&mut self) {
        if let Some(content) = &self.current_generating_content {
            if !content.is_empty() {
                self.reasoning_summary_items
                    .push(llm::ReasoningSummaryItem::SummaryText {
                        text: content.clone(),
                    });
            }
        }

        self.current_generating_content = Some(String::new());
        self.current_generating_title = None;
    }

    /// Append delta text to the current reasoning summary item
    pub fn append_reasoning_summary_delta(&mut self, delta: String) {
        if self.current_generating_content.is_none() {
            self.current_generating_content = Some(String::new());
        }

        if let Some(content) = &mut self.current_generating_content {
            content.push_str(&delta);
            self.current_generating_title = Self::parse_title_from_content(content);
        }
    }

    /// Complete reasoning and finalize any remaining items
    pub fn complete_reasoning(&mut self) {
        // Finalize current item if any
        if let Some(content) = &self.current_generating_content {
            if !content.is_empty() {
                self.reasoning_summary_items
                    .push(llm::ReasoningSummaryItem::SummaryText {
                        text: content.clone(),
                    });
            }
        }

        // Clear current state
        self.current_generating_title = None;
        self.current_generating_content = None;

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
        }
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
        let result = if is_generating {
            // While generating, show current item content
            let content = self
                .current_generating_content
                .as_deref()
                .unwrap_or(&self.content)
                .to_string();
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

            // Fallback: if reasoning_summary_items is empty but we had content,
            // there might have been a timing issue during completion
            if reasoning_content.is_empty() && !self.content.is_empty() {
                self.content.clone()
            } else {
                reasoning_content
            }
        } else {
            // Traditional thinking block
            self.content.clone()
        };

        result
    }

    /// Check if this is a reasoning block (has reasoning summary items)
    pub fn is_reasoning_block(&self) -> bool {
        !self.reasoning_summary_items.is_empty() || self.current_generating_content.is_some()
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
