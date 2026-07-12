//! `MessageContainer` — holds the block entities for a single message row.
//!
//! Each message (user turn, assistant turn, or system divider) in the messages
//! list is backed by one `MessageContainer` entity. It owns a vector of
//! `Entity<BlockView>` and exposes mutation methods that the UI event loop
//! calls during streaming (add text, add tool, append, etc.).

use code_assistant_core::persistence::{BranchInfo, NodeId};

use crate::shared::image;
use code_assistant_core::ui::ToolStatus;
use gpui::{prelude::*, Context, Entity};
use std::sync::{Arc, Mutex};
use tracing::{debug, trace, warn};

use super::{
    BlockData, BlockView, CompactionSummaryBlock, ImageBlock, MessageRole, ParameterBlock,
    TextBlock, ThinkingBlock, ToolBlockState, ToolCollapseState, ToolUseBlock,
};

/// Tracks the last block type for paragraph breaks after hidden tools
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HiddenToolBlockType {
    Text,
    Thinking,
}

/// Container for all elements within a single message (one LLM request/response).
/// Each MessageContainer maps to exactly one item in the virtualized list,
/// enabling efficient scroll virtualization.
#[derive(Clone)]
pub struct MessageContainer {
    elements: Arc<Mutex<Vec<Entity<BlockView>>>>,
    role: MessageRole,
    /// The request_id identifying which LLM request produced this container's blocks.
    /// Used to remove blocks when a request is cancelled or rolled back.
    current_request_id: Arc<Mutex<u64>>,
    /// Current project for parameter filtering (used to detect cross-project tool calls)
    current_project: Arc<Mutex<String>>,
    /// Tracks the last block type for hidden tool paragraph breaks
    last_block_type_for_hidden_tool: Arc<Mutex<Option<HiddenToolBlockType>>>,
    /// Flag indicating a hidden tool completed and we may need a paragraph break
    needs_paragraph_break_after_hidden_tool: Arc<Mutex<bool>>,
    /// Node ID for this message (for branching support)
    node_id: Arc<Mutex<Option<NodeId>>>,
    /// Branch info if this message is part of a branch point
    branch_info: Arc<Mutex<Option<BranchInfo>>>,
    /// Session ID this container belongs to, used by tool blocks to read/write
    /// the global [`ToolCollapseState`] registry.
    session_id: Arc<Mutex<Option<String>>>,
    /// Monotonic block identifier source for stable per-block view state.
    next_block_id: Arc<Mutex<u64>>,
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
            session_id: Arc::new(Mutex::new(None)),
            next_block_id: Arc::new(Mutex::new(0)),
        }
    }

    fn allocate_block_id(&self) -> u64 {
        let mut next = self.next_block_id.lock().unwrap();
        let id = *next;
        *next += 1;
        id
    }

    // Set the current request ID for this message container
    pub fn set_current_request_id(&self, request_id: u64) {
        *self.current_request_id.lock().unwrap() = request_id;
    }

    /// Set the session ID for this container (used for collapse-state tracking).
    pub fn set_session_id(&self, session_id: Option<String>) {
        *self.session_id.lock().unwrap() = session_id;
    }

    /// Get the session ID for this container.
    #[allow(dead_code)]
    pub fn session_id(&self) -> Option<String> {
        self.session_id.lock().unwrap().clone()
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

    /// Check if this is a system message (e.g. compaction dividers)
    #[allow(dead_code)]
    pub fn is_system_message(&self) -> bool {
        self.role == MessageRole::System
    }

    pub fn elements(&self) -> Vec<Entity<BlockView>> {
        let elements = self.elements.lock().unwrap();
        elements.clone()
    }

    /// Returns true if this container has no block elements.
    pub fn is_empty(&self) -> bool {
        self.elements.lock().unwrap().is_empty()
    }

    // Add a new text block
    pub fn add_text_block(&self, content: impl Into<String>, cx: &mut Context<Self>) {
        self.finish_any_thinking_blocks(cx);

        let request_id = *self.current_request_id.lock().unwrap();
        let block_id = self.allocate_block_id();
        let mut elements = self.elements.lock().unwrap();
        let block = BlockData::TextBlock(TextBlock {
            content: content.into(),
        });
        let view = cx.new(|cx| {
            BlockView::new(
                block,
                block_id,
                request_id,
                self.current_project.clone(),
                None,
                cx,
            )
        });
        elements.push(view);
        cx.notify();
    }

    pub fn add_compaction_divider(&self, summary: impl Into<String>, cx: &mut Context<Self>) {
        self.finish_any_thinking_blocks(cx);

        let request_id = *self.current_request_id.lock().unwrap();
        let block_id = self.allocate_block_id();
        let mut elements = self.elements.lock().unwrap();
        let block = BlockData::CompactionSummary(CompactionSummaryBlock {
            summary: summary.into(),
            is_expanded: false,
        });
        let view = cx.new(|cx| {
            BlockView::new(
                block,
                block_id,
                request_id,
                self.current_project.clone(),
                None,
                cx,
            )
        });
        elements.push(view);
        cx.notify();
    }

    // Add a new thinking block
    #[allow(dead_code)]
    pub fn add_thinking_block(&self, content: impl Into<String>, cx: &mut Context<Self>) {
        self.finish_any_thinking_blocks(cx);

        let request_id = *self.current_request_id.lock().unwrap();
        let block_id = self.allocate_block_id();
        let mut elements = self.elements.lock().unwrap();
        let block = BlockData::ThinkingBlock(ThinkingBlock::new(content.into()));
        let view = cx.new(|cx| {
            BlockView::new(
                block,
                block_id,
                request_id,
                self.current_project.clone(),
                None,
                cx,
            )
        });
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
        let block_id = self.allocate_block_id();
        let mut elements = self.elements.lock().unwrap();
        let block = BlockData::ImageBlock(ImageBlock { media_type, image });
        let view = cx.new(|cx| {
            BlockView::new(
                block,
                block_id,
                request_id,
                self.current_project.clone(),
                None,
                cx,
            )
        });
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
        self.add_tool_use_block_with_duration(name, id, None, cx);
    }

    /// Add a new tool use block with optional pre-computed duration from persisted ContentBlock timestamps
    pub fn add_tool_use_block_with_duration(
        &self,
        name: impl Into<String>,
        id: impl Into<String>,
        duration_seconds: Option<f64>,
        cx: &mut Context<Self>,
    ) {
        self.finish_any_thinking_blocks(cx);

        let request_id = *self.current_request_id.lock().unwrap();
        let block_id = self.allocate_block_id();
        let mut elements = self.elements.lock().unwrap();
        let name = name.into();
        let id = id.into();
        let session_id = self.session_id.lock().unwrap().clone();

        // Check the global collapse-state registry for a user override first.
        // If the user previously toggled this tool block, honour that choice.
        // Otherwise fall back to the renderer default (Card → Expanded, Inline → Collapsed).
        let initial_state = if let Some(override_state) = session_id
            .as_deref()
            .and_then(|sid| ToolCollapseState::get(sid, &id))
        {
            override_state
        } else {
            // No renderer (unknown tool) → collapsed; otherwise ask the
            // renderer (cards expanded by default, browser cards collapsed).
            let starts_collapsed = crate::tool_cards::ToolBlockRendererRegistry::global()
                .as_ref()
                .and_then(|registry| registry.get(&name).cloned())
                .map(|r| r.starts_collapsed())
                .unwrap_or(true);
            if starts_collapsed {
                ToolBlockState::Collapsed
            } else {
                ToolBlockState::Expanded
            }
        };

        let block = BlockData::ToolUse(ToolUseBlock {
            name,
            id,
            parameters: Vec::new(),
            status: ToolStatus::Pending,
            status_message: None,
            output: None,
            styled_output: None,
            state: initial_state,
            duration_seconds,
            images: Vec::new(),
        });
        let view = cx.new(|cx| {
            BlockView::new(
                block,
                block_id,
                request_id,
                self.current_project.clone(),
                session_id,
                cx,
            )
        });
        elements.push(view);
        cx.notify();
    }

    // Update the status of a tool block
    #[allow(clippy::too_many_arguments)]
    pub fn update_tool_status(
        &self,
        tool_id: &str,

        status: ToolStatus,
        message: Option<String>,
        output: Option<String>,
        styled_output: Option<Vec<terminal::StyledLine>>,
        duration_seconds: Option<f64>,
        images: Vec<(String, String)>,
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

                        // Update styled output if provided (terminal color data)
                        if styled_output.is_some() {
                            tool.styled_output = styled_output.clone();
                        }

                        // Store duration from ContentBlock timestamps (stable across restores)
                        if duration_seconds.is_some() {
                            tool.duration_seconds = duration_seconds;
                        }

                        // Store image data from tools that produce visual output
                        if !images.is_empty() {
                            tool.images = images.clone();
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
                    let appended_text = if let Some(prefix) = &paragraph_prefix {
                        format!("{}{}", prefix, content)
                    } else {
                        content.clone()
                    };
                    text_block.content.push_str(&appended_text);
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
        let block_id = self.allocate_block_id();
        let final_content = if let Some(prefix) = paragraph_prefix {
            format!("{}{}", prefix, content)
        } else {
            content.to_string()
        };
        let block = BlockData::TextBlock(TextBlock {
            content: final_content,
        });
        let view = cx.new(|cx| {
            BlockView::new(
                block,
                block_id,
                request_id,
                self.current_project.clone(),
                None,
                cx,
            )
        });
        elements.push(view);
        cx.notify();
    }

    // Add or append to thinking block
    pub fn add_or_append_to_thinking_block(
        &self,
        content: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.add_or_append_to_thinking_block_with_duration(content, None, cx);
    }

    /// Add or append to thinking block, with optional pre-computed duration from persisted timestamps
    pub fn add_or_append_to_thinking_block_with_duration(
        &self,
        content: impl Into<String>,
        duration_seconds: Option<f64>,
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
                    let appended_text = if let Some(prefix) = &paragraph_prefix {
                        format!("{}{}", prefix, content)
                    } else {
                        content.clone()
                    };
                    thinking_block.content.push_str(&appended_text);
                    // Store duration if provided (from session restore)
                    if duration_seconds.is_some() {
                        thinking_block.duration_seconds = duration_seconds;
                        thinking_block.is_completed = true;
                        view.set_generating(false);
                    }
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
        let block_id = self.allocate_block_id();
        let final_content = if let Some(prefix) = paragraph_prefix {
            format!("{}{}", prefix, content)
        } else {
            content.to_string()
        };

        let has_duration = duration_seconds.is_some();
        let mut thinking = ThinkingBlock::new(final_content);
        if let Some(dur) = duration_seconds {
            thinking.duration_seconds = Some(dur);
            thinking.is_completed = true;
        }
        let block = BlockData::ThinkingBlock(thinking);
        let view = cx.new(|cx| {
            let mut bv = BlockView::new(
                block,
                block_id,
                request_id,
                self.current_project.clone(),
                None,
                cx,
            );
            if has_duration {
                bv.set_generating(false);
            }
            bv
        });
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
            warn!(
                "GPUI add_or_update_tool_parameter: missing tool block for tool_id='{}', param='{}' — creating fallback block",
                tool_id, name
            );
            let request_id = *self.current_request_id.lock().unwrap();
            let session_id = self.session_id.lock().unwrap().clone();

            // Check the global collapse registry for a user override
            let initial_state = session_id
                .as_deref()
                .and_then(|sid| ToolCollapseState::get(sid, &tool_id))
                .unwrap_or(ToolBlockState::Collapsed);

            let mut tool = ToolUseBlock {
                name: "unknown".to_string(), // Default name since we only have ID
                id: tool_id.clone(),
                parameters: Vec::new(),
                status: ToolStatus::Pending,
                status_message: None,
                output: None,
                styled_output: None,
                state: initial_state,
                duration_seconds: None,
                images: Vec::new(),
            };

            tool.parameters.push(ParameterBlock {
                name: name.clone(),
                value: value.clone(),
            });

            let block = BlockData::ToolUse(tool);
            let block_id = self.allocate_block_id();
            let view = cx.new(|cx| {
                BlockView::new(
                    block,
                    block_id,
                    request_id,
                    self.current_project.clone(),
                    session_id,
                    cx,
                )
            });
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

        debug!(
            "GPUI replace_tool_parameter: tool block not found for tool_id='{}', param='{}'",
            tool_id, name
        );
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
        let mut found = false;

        // Find the tool and append the output chunk
        for element in elements.iter() {
            cx.update_entity(element, |block_view, cx| {
                if let Some(tool_block) = block_view.block.as_tool_mut() {
                    if tool_block.id == tool_id {
                        found = true;
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

        if !found {
            warn!(
                "GPUI append_tool_output: tool block not found for tool_id='{}', chunk_len={}",
                tool_id,
                chunk.len()
            );
        }
    }

    pub fn finish_any_thinking_blocks(&self, cx: &mut Context<Self>) {
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
        let block_id = self.allocate_block_id();
        let mut new_thinking_block = ThinkingBlock::new(String::new());
        new_thinking_block.start_reasoning_summary_item();

        let block = BlockData::ThinkingBlock(new_thinking_block);
        let view = cx.new(|cx| {
            BlockView::new(
                block,
                block_id,
                request_id,
                self.current_project.clone(),
                None,
                cx,
            )
        });
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
        let block_id = self.allocate_block_id();
        let mut new_thinking_block = ThinkingBlock::new(String::new());
        new_thinking_block.start_reasoning_summary_item();
        new_thinking_block.append_reasoning_summary_delta(delta);

        let block = BlockData::ThinkingBlock(new_thinking_block);
        let view = cx.new(|cx| {
            BlockView::new(
                block,
                block_id,
                request_id,
                self.current_project.clone(),
                None,
                cx,
            )
        });
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
