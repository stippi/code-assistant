# Context Window Management Implementation Plan

## Overview

This feature implements automatic context window management for the agent loop. When the context window approaches a configurable threshold (e.g., 85% full), the system will automatically interrupt the LLM with a system-generated message requesting a comprehensive progress summary. After receiving the summary, the system **marks old messages as archived** (but keeps them in storage and UI) and uses only the summary plus new messages for subsequent LLM requests, effectively creating a "fresh start" with context continuity.

### Key Design Principles

1. **Preserve Everything**: All messages are kept in storage and shown in the UI. Nothing is deleted.

2. **Archive, Don't Delete**: Messages before a compaction point are "archived" - they remain visible but aren't sent to the LLM.

3. **Visual Compaction Markers**: The UI displays special expandable markers at each compaction point showing the summary and statistics.

4. **Agent Sees Only Active Context**: The agent loop only processes messages after the last compaction point, keeping the LLM context fresh.

5. **Session Continuity**: When resuming a session, users see the full conversation history with compaction markers, but the agent continues from the last compaction point.

## Motivation

Large coding tasks with extensive tool usage can quickly fill up the context window. Currently, when the context limit is reached, the LLM request fails. This feature provides a graceful degradation mechanism that:
- Prevents hard failures from context overflow
- Maintains task continuity through LLM-generated summaries
- Allows long-running tasks to complete successfully
- Reduces token costs by archiving old messages
- Preserves full conversation history for user reference

## Visual Summary

```
┌─────────────────────────────────────────────────────────────┐
│                     CONTEXT COMPACTION                       │
│                                                               │
│  BEFORE THRESHOLD (85% of context limit):                   │
│  Agent sends: [msg1, msg2, msg3, ..., msg44, msg45]        │
│               └─────────── ALL MESSAGES ──────────┘          │
│                                                               │
│  AT THRESHOLD:                                               │
│  1. System requests summary from LLM                         │
│  2. LLM provides comprehensive summary                       │
│  3. CompactionRecord created at message index 45             │
│  4. Summary stored as first message of new segment           │
│                                                               │
│  AFTER COMPACTION:                                           │
│  Storage:    [msg1, ..., msg44, msg45, summary, msg46, ...] │
│               └─ archived ─┘         └──── active ────┘      │
│                                                               │
│  Agent sees: [summary, msg46, msg47, ...]                   │
│               └───── ONLY ACTIVE MESSAGES ─────┘             │
│                                                               │
│  UI shows:   [msg1, ..., msg44]                             │
│               [📦 Compaction Marker #1 - Expandable]         │
│               [msg45, summary, msg46, ...]                   │
│               └─────── ALL MESSAGES + MARKER ───────┘        │
└─────────────────────────────────────────────────────────────┘
```

## Architecture

### 1. Context Tracking

**Location**: `crates/code_assistant/src/agent/runner.rs` (Agent struct)

The agent already has access to message usage data through `Message.usage`. We'll leverage this to track context size:

```rust
pub struct Agent {
    // ... existing fields ...

    /// Context window configuration
    context_config: ContextWindowConfig,

    /// Count of context resets that have occurred in this session
    context_reset_count: u32,
}

#[derive(Debug, Clone)]
pub struct ContextWindowConfig {
    /// Maximum context window size in tokens (from model config)
    pub limit: Option<u32>,

    /// Threshold percentage (0.0-1.0) at which to trigger summary
    /// Default: 0.85 (85%)
    pub threshold: f32,

    /// Whether context management is enabled
    pub enabled: bool,
}
```

**Context Size Calculation**:
- Use `input_tokens + cache_read_input_tokens` from the most recent assistant message
- This represents the total tokens being processed in the current LLM request
- Already implemented in `SessionInstance::get_current_context_size()`

### 2. Model Configuration

**Location**: `crates/llm/src/provider_config.rs`

Add `context_limit` field to `ModelConfig`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub provider: String,
    pub id: String,
    pub config: serde_json::Value,

    /// Optional context window limit in tokens
    /// If not specified, no automatic context management is performed
    #[serde(default)]
    pub context_limit: Option<u32>,
}
```

**Example models.json**:
```json
{
  "Claude Sonnet 4.5": {
    "provider": "anthropic-main",
    "id": "claude-sonnet-4-5",
    "context_limit": 200000,
    "config": {
      "max_tokens": 32768,
      "thinking": {
        "type": "enabled",
        "budget_tokens": 8192
      }
    }
  },
  "GPT-4.1": {
    "provider": "openai-main",
    "id": "gpt-4.1",
    "context_limit": 128000,
    "config": {
      "temperature": 0.8,
      "max_tokens": 4096
    }
  }
}
```

### 3. Session Configuration

**Location**: `crates/code_assistant/src/session/mod.rs`

Add context management settings to `SessionConfig`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionConfig {
    // ... existing fields ...

    /// Context window threshold (0.0-1.0) for triggering summary
    /// Default: 0.85 (85%)
    #[serde(default = "default_context_threshold")]
    pub context_threshold: f32,

    /// Whether to enable automatic context window management
    /// Default: true
    #[serde(default = "default_context_management_enabled")]
    pub context_management_enabled: bool,
}

fn default_context_threshold() -> f32 {
    0.85
}

fn default_context_management_enabled() -> bool {
    true
}
```

### 4. Context Window Check

**Location**: `crates/code_assistant/src/agent/runner.rs`

Add check before LLM requests in `get_next_assistant_message()`:

```rust
impl Agent {
    /// Check if context window is approaching limit
    fn should_request_summary(&self) -> bool {
        // Only if context management is enabled
        if !self.context_config.enabled {
            return false;
        }

        // Need a context limit to check against
        let limit = match self.context_config.limit {
            Some(limit) => limit,
            None => return false,
        };

        // Calculate current context size from last assistant message
        let current_size = self.get_current_context_size();

        // Check if we're over the threshold
        let threshold_tokens = (limit as f32 * self.context_config.threshold) as u32;

        debug!(
            "Context check: {}/{} tokens (threshold: {})",
            current_size, limit, threshold_tokens
        );

        current_size >= threshold_tokens
    }

    /// Get current context size from last assistant message
    fn get_current_context_size(&self) -> u32 {
        for message in self.message_history.iter().rev() {
            if matches!(message.role, MessageRole::Assistant) {
                if let Some(usage) = &message.usage {
                    return usage.input_tokens + usage.cache_read_input_tokens;
                }
            }
        }
        0
    }
}
```

### 5. Summary Request

**Location**: `crates/code_assistant/src/agent/runner.rs`

When threshold is reached, inject a system message requesting summary:

```rust
impl Agent {
    /// Request a progress summary from the LLM
    async fn request_context_summary(&mut self) -> Result<String> {
        info!("Context window approaching limit, requesting summary");

        let summary_request = Message {
            role: MessageRole::User,
            content: MessageContent::Text(
                self.generate_summary_request_message()
            ),
            request_id: None,
            usage: None,
        };

        // Add to message history
        self.append_message(summary_request)?;

        // Notify UI about context management event
        self.ui.send_event(UiEvent::ContextWindowManagement {
            event: ContextManagementEvent::SummaryRequested,
            current_size: self.get_current_context_size(),
            limit: self.context_config.limit,
        }).await?;

        // Get LLM response with the full context
        let messages = self.render_tool_results_in_messages();
        let (llm_response, request_id) = self.get_next_assistant_message(messages).await?;

        // Extract text summary from response
        let summary = self.extract_text_from_response(&llm_response)?;

        // Add the summary message to history (it will be preserved)
        self.append_message(Message {
            role: MessageRole::Assistant,
            content: MessageContent::Text(summary.clone()),
            request_id: Some(request_id),
            usage: Some(llm_response.usage.clone()),
        })?;

        Ok(summary)
    }

    /// Generate the system message requesting a summary
    fn generate_summary_request_message(&self) -> String {
        format!(
            "<system-context-management>\n\
            The context window is approaching its limit. Before we continue, please provide \
            a COMPLETE and DETAILED summary of:\n\
            \n\
            1. **Original Task**: What was the user's original request?\n\
            2. **Progress Made**: What have you accomplished so far? Include:\n\
               - Files created, modified, or analyzed\n\
               - Tools used and their results\n\
               - Problems encountered and solutions applied\n\
               - Current state of the codebase\n\
            3. **Working Memory**: What key information needs to be preserved?\n\
               - Project structure and important files\n\
               - Dependencies and configurations\n\
               - Patterns or conventions discovered\n\
            4. **Next Steps**: What remains to be done?\n\
               - Pending tasks from the plan\n\
               - Known issues to address\n\
               - Next logical steps\n\
            \n\
            This summary will be used to continue the task in a fresh context. \
            Be thorough and specific - include file names, code snippets, and concrete details. \
            The more complete your summary, the better you can continue after the context reset.\n\
            \n\
            NOTE: After you provide this summary, the message history will be cleared \
            and only your summary will be preserved for the next round. Do NOT use any tools \
            in this response - just provide the comprehensive summary as plain text.\n\
            </system-context-management>"
        )
    }

    /// Extract text content from LLM response
    fn extract_text_from_response(&self, response: &llm::LLMResponse) -> Result<String> {
        let mut text = String::new();

        for block in &response.content {
            if let ContentBlock::Text { text: block_text, .. } = block {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(block_text);
            }
        }

        if text.trim().is_empty() {
            anyhow::bail!("LLM did not provide a text summary");
        }

        Ok(text)
    }
}
```

### 6. Context Compaction as ContentBlock

**Key Design Change**: Use a special `ContentBlock::ContextCompaction` type to mark compaction boundaries directly in the message history. This is more robust than using indices because:
- Self-documenting: the compaction data is in the message itself
- Index-independent: works even if messages are removed or edited
- Simpler logic: just scan for compaction blocks
- Easy to serialize/deserialize

**Location**: `crates/llm/src/types.rs`

Add new content block type:

```rust
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
#[serde(tag = "type")]
pub enum ContentBlock {
    // ... existing variants ...

    #[serde(rename = "context_compaction")]
    ContextCompaction {
        /// Compaction number (1st, 2nd, 3rd, etc.)
        compaction_number: u32,

        /// When the compaction occurred
        timestamp: SystemTime,

        /// The summary provided by the LLM
        summary: String,

        /// Number of messages that were archived
        messages_archived: usize,

        /// Context size before compaction
        context_size_before: u32,
    },
}

impl ContentBlock {
    pub fn new_context_compaction(
        compaction_number: u32,
        summary: String,
        messages_archived: usize,
        context_size_before: u32,
    ) -> Self {
        ContentBlock::ContextCompaction {
            compaction_number,
            timestamp: SystemTime::now(),
            summary,
            messages_archived,
            context_size_before,
        }
    }
}
```

**Location**: `crates/code_assistant/src/agent/runner.rs`

```rust
pub struct Agent {
    // ... existing fields ...

    // NO LONGER NEEDED: compaction_records field
    // Compaction data is now stored inline in messages
}

impl Agent {
    /// Compact the context window by adding a compaction message
    async fn compact_context_window(&mut self, summary: String) -> Result<()> {
        info!("Compacting context window");

        let compaction_number = self.count_compactions() + 1;
        let messages_archived = self.message_history.len();
        let context_size_before = self.get_current_context_size();

        // Create compaction block
        let compaction_block = ContentBlock::new_context_compaction(
            compaction_number,
            summary.clone(),
            messages_archived,
            context_size_before,
        );

        // Add as a User message with the compaction block plus instruction text
        let compaction_message = Message {
            role: MessageRole::User,
            content: MessageContent::Structured(vec![
                compaction_block.clone(),
                ContentBlock::new_text(format!(
                    "The context has been compacted. Continue the task based on the summary above. \
                    You can now use tools again."
                )),
            ]),
            request_id: None,
            usage: None,
        };

        self.append_message(compaction_message)?;

        // Invalidate system message cache to regenerate with new context
        self.invalidate_system_message_cache();

        // Notify UI about the compaction
        self.ui.send_event(UiEvent::ContextCompacted {
            compaction_number,
            messages_archived,
            context_size_before,
            summary: summary.clone(),
        }).await?;

        info!(
            "Context compaction complete: archived {} messages, compaction #{}",
            messages_archived, compaction_number
        );

        Ok(())
    }

    /// Count how many compactions have occurred by scanning messages
    fn count_compactions(&self) -> u32 {
        self.message_history
            .iter()
            .flat_map(|msg| match &msg.content {
                MessageContent::Structured(blocks) => blocks.iter().collect::<Vec<_>>(),
                _ => vec![],
            })
            .filter(|block| matches!(block, ContentBlock::ContextCompaction { .. }))
            .count() as u32
    }

    /// Get only the active messages (after last compaction point)
    fn get_active_messages(&self) -> Vec<Message> {
        // Find the last message with a ContextCompaction block
        let last_compaction_index = self.message_history
            .iter()
            .enumerate()
            .rev()
            .find(|(_, msg)| {
                match &msg.content {
                    MessageContent::Structured(blocks) => {
                        blocks.iter().any(|block| {
                            matches!(block, ContentBlock::ContextCompaction { .. })
                        })
                    }
                    _ => false,
                }
            })
            .map(|(index, _)| index);

        match last_compaction_index {
            Some(index) => {
                // Return messages starting from the compaction message
                self.message_history[index..].to_vec()
            }
            None => {
                // No compaction yet, all messages are active
                self.message_history.clone()
            }
        }
    }

    /// Extract all compaction blocks from message history for UI display
    pub fn get_compaction_data(&self) -> Vec<CompactionData> {
        self.message_history
            .iter()
            .enumerate()
            .filter_map(|(index, msg)| {
                match &msg.content {
                    MessageContent::Structured(blocks) => {
                        blocks.iter().find_map(|block| {
                            if let ContentBlock::ContextCompaction {
                                compaction_number,
                                timestamp,
                                summary,
                                messages_archived,
                                context_size_before,
                            } = block {
                                Some(CompactionData {
                                    message_index: index,
                                    compaction_number: *compaction_number,
                                    timestamp: *timestamp,
                                    summary: summary.clone(),
                                    messages_archived: *messages_archived,
                                    context_size_before: *context_size_before,
                                })
                            } else {
                                None
                            }
                        })
                    }
                    _ => None,
                }
            })
            .collect()
    }
}

/// UI-friendly representation of compaction data
#[derive(Debug, Clone)]
pub struct CompactionData {
    pub message_index: usize,
    pub compaction_number: u32,
    pub timestamp: SystemTime,
    pub summary: String,
    pub messages_archived: usize,
    pub context_size_before: u32,
}
```
```

### 7. Integration in Agent Loop

**Location**: `crates/code_assistant/src/agent/runner.rs`

Integrate the check in the main agent loop and modify message rendering:

```rust
impl Agent {
    pub async fn run_single_iteration(&mut self) -> Result<()> {
        loop {
            // Check for pending user message
            if let Some(pending_message) = self.get_and_clear_pending_message() {
                // ... existing pending message handling ...
            }

            // Check if we need to request a summary BEFORE making the LLM request
            if self.should_request_summary() {
                // Request and get summary from LLM
                let summary = self.request_context_summary().await?;

                // Compact the context window (mark old messages as archived)
                self.compact_context_window(summary).await?;

                // Continue the loop with fresh context
                continue;
            }

            // Prepare messages for LLM (only active messages after last compaction)
            let messages = self.render_tool_results_in_messages();

            // Get LLM response (existing logic)
            let (llm_response, request_id) = self.get_next_assistant_message(messages).await?;

            // ... rest of existing loop logic ...
        }
    }

    /// Prepare messages for LLM request, dynamically rendering tool outputs
    /// MODIFIED: Only return active messages (after last compaction point)
    fn render_tool_results_in_messages(&self) -> Vec<Message> {
        // Get only active messages
        let active_messages = self.get_active_messages();

        // Start with a clean slate
        let mut messages = Vec::new();

        // Create a fresh ResourcesTracker for this rendering pass
        let mut resources_tracker = crate::tools::core::render::ResourcesTracker::new();

        // Build map from tool_use_id to rendered output
        // IMPORTANT: Only include tool executions that correspond to active messages
        let mut tool_outputs = std::collections::HashMap::new();

        // Determine which tool executions are still active
        // For now, include all - alternatively could track which are archived
        for execution in self.tool_executions.iter().rev() {
            let tool_use_id = &execution.tool_request.id;
            let rendered_output = execution.result.as_render().render(&mut resources_tracker);
            tool_outputs.insert(tool_use_id.clone(), rendered_output);
        }

        // Rebuild the message history from active messages only
        for msg in active_messages {
            // ... existing message rendering logic ...
        }

        messages
    }
}
```

### 8. Persistence

**No changes needed!** Compaction data is now stored directly in the `messages` field as special `ContentBlock::ContextCompaction` blocks. The existing persistence layer automatically serializes/deserializes them along with all other content blocks.

This is a major simplification:
- No new fields needed in `ChatSession`
- No new fields needed in `SessionState`
- No special handling in persistence layer
- Compaction data travels with messages automatically
- JSON format is self-documenting

Example persisted message with compaction:
```json
{
  "role": "user",
  "content": [
    {
      "type": "context_compaction",
      "compaction_number": 1,
      "timestamp": "2024-01-15T10:30:00Z",
      "summary": "Original task was...",
      "messages_archived": 45,
      "context_size_before": 150000
    },
    {
      "type": "text",
      "text": "The context has been compacted. Continue based on the summary above."
    }
  ]
}
```

### 9. UI Events and Display

**Location**: `crates/code_assistant/src/ui/ui_events.rs`

Add new UI event for context management:

```rust
pub enum UiEvent {
    // ... existing events ...

    /// Context was compacted
    ContextCompacted {
        compaction_number: u32,
        messages_archived: usize,
        context_size_before: u32,
        summary: String,
    },

    /// Context usage update (optional, for progress indicator)
    ContextUsageUpdate {
        current_size: u32,
        limit: u32,
        percentage: f32,
    },
}
```

**Location**: `crates/code_assistant/src/ui/gpui/elements.rs` (or similar)

Create a new UI element for displaying compaction blocks:

```rust
/// UI element to display a context compaction event in the chat
pub struct CompactionMarker {
    compaction_data: CompactionData,
    is_expanded: bool,
}

impl CompactionMarker {
    pub fn new(data: CompactionData) -> Self {
        Self {
            compaction_data: data,
            is_expanded: false,
        }
    }

    /// Render the compaction marker in the chat view
    pub fn render(&self, cx: &mut ViewContext<ChatView>) -> impl IntoElement {
        div()
            .bg(cx.theme().colors().surface_variant) // Different background
            .rounded_md()
            .p_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(Icon::new(IconName::Archive))
                            .child(
                                Label::new(format!(
                                    "Context Compacted #{} — {} messages archived, {} tokens freed",
                                    self.compaction_data.compaction_number,
                                    self.compaction_data.messages_archived,
                                    self.compaction_data.context_size_before
                                ))
                                .color(cx.theme().colors().text_muted)
                            )
                    )
                    .child(
                        IconButton::new("expand", IconName::ChevronDown)
                            .on_click(cx.listener(|this, _, cx| {
                                this.is_expanded = !this.is_expanded;
                                cx.notify();
                            }))
                    )
            )
            .when(self.is_expanded, |el| {
                el.child(
                    div()
                        .mt_2()
                        .pt_2()
                        .border_t_1()
                        .border_color(cx.theme().colors().border)
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().colors().text)
                                .child(Label::new("Summary from LLM:"))
                        )
                        .child(
                            div()
                                .mt_2()
                                .p_2()
                                .bg(cx.theme().colors().surface)
                                .rounded_md()
                                .text_sm()
                                .whitespace_pre_wrap()
                                .child(self.compaction_data.summary.clone())
                        )
                )
            })
    }
}
```

**Location**: `crates/code_assistant/src/ui/gpui/messages.rs` (or chat view)

Integrate compaction markers into the message list:

```rust
impl ChatView {
    /// Render the message list with compaction markers
    fn render_messages(&self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        let mut elements = Vec::new();

        // Render messages, detecting compaction blocks within them
        for message_data in &self.messages {
            // Check if this message contains a ContextCompaction block
            let compaction_data = self.extract_compaction_from_message(message_data);

            if let Some(data) = compaction_data {
                // Render compaction marker instead of regular message
                elements.push(CompactionMarker::new(data).render(cx));
            } else {
                // Render the actual message normally
                elements.push(self.render_message(message_data, cx));
            }
        }

        // Render all elements in a vertical list
        v_flex().gap_2().children(elements)
    }

    /// Extract compaction data from a message if it contains a ContextCompaction block
    fn extract_compaction_from_message(&self, message_data: &MessageData) -> Option<CompactionData> {
        // Look through fragments for a compaction block
        // This depends on how MessageData is structured from the streaming processor
        // The compaction block would have been converted to a DisplayFragment

        // Alternatively, keep the session's agent compaction data and correlate by position
        // Or have the SessionInstance provide compaction data separately
        None // Placeholder - actual implementation depends on UI data structures
    }
}
```

**Note**: The exact implementation depends on how `MessageData` and `DisplayFragment` are structured. You may need to:
1. Add compaction handling to the stream processor so it converts `ContextCompaction` blocks to special fragments
2. Or have `SessionInstance` provide compaction data separately via `get_compaction_data()` and correlate by message index
3. Or add a field to `MessageData` to indicate it's a compaction message

### 10. Configuration Loading

**Location**: `crates/code_assistant/src/agent/runner.rs`

Load context config when creating agent:

```rust
impl Agent {
    pub fn new(components: AgentComponents, session_config: SessionConfig) -> Self {
        // ... existing initialization ...

        let context_config = ContextWindowConfig {
            limit: None, // Will be set when model is loaded
            threshold: session_config.context_threshold,
            enabled: session_config.context_management_enabled,
        };

        Self {
            // ... existing fields ...
            context_config,
            context_reset_count: 0,
        }
    }

    /// Update context config when model changes
    pub fn set_model_config(&mut self, model_config: &SessionModelConfig,
                           provider_config: &llm::provider_config::ModelConfig) {
        self.context_config.limit = provider_config.context_limit;

        debug!(
            "Updated context config: limit={:?}, threshold={:.2}%, enabled={}",
            self.context_config.limit,
            self.context_config.threshold * 100.0,
            self.context_config.enabled
        );
    }
}
```

Also need to pass the context limit from SessionModelConfig through to the Agent:

**Location**: `crates/code_assistant/src/session/manager.rs`

```rust
impl SessionManager {
    pub async fn start_agent_for_message(&mut self, ...) -> Result<()> {
        // ... existing code ...

        let mut agent = Agent::new(components, session_config.clone());

        // Load model config and set context limit
        if let Some(model_config) = &session_state.model_config {
            let config_system = llm::provider_config::ConfigurationSystem::load()?;
            if let Some(provider_model_config) = config_system.get_model(&model_config.model_name) {
                agent.set_model_config(model_config, provider_model_config);
            }
        }

        // ... rest of existing code ...
    }
}
```

## UI/UX Flow

### Chat View with Compaction

```
┌─────────────────────────────────────────────────────────────┐
│  User: Please implement feature X                           │
│                                                               │
│  Assistant: I'll help you implement that...                  │
│  [Uses tools, creates files, etc.]                           │
│                                                               │
│  User: Now add tests                                         │
│                                                               │
│  Assistant: Adding tests...                                  │
│  [More tools, more messages...]                              │
│                                                               │
│  ┌───────────────────────────────────────────────────────┐  │
│  │ 🗃️  Context Compacted #1                              │  │
│  │ 45 messages archived, ~150,000 tokens freed           │  │
│  │                                              [⌄ Expand]│  │
│  └───────────────────────────────────────────────────────┘  │
│                                                               │
│  User: Can you also add documentation?                       │
│                                                               │
│  Assistant: Continuing from the summary...                   │
│  [Uses tools with fresh context]                             │
│                                                               │
└─────────────────────────────────────────────────────────────┘
```

### Expanded Compaction Marker

```
┌───────────────────────────────────────────────────────────────┐
│ 🗃️  Context Compacted #1                                      │
│ 45 messages archived, ~150,000 tokens freed                   │
│                                                 [⌃ Collapse]   │
│ ─────────────────────────────────────────────────────────────  │
│ Summary from LLM:                                             │
│                                                                │
│ ┌─────────────────────────────────────────────────────────┐  │
│ │ **Original Task**: Implement feature X with tests       │  │
│ │                                                          │  │
│ │ **Progress Made**:                                       │  │
│ │ - Created main implementation in src/feature.rs          │  │
│ │ - Added configuration handling in config.rs              │  │
│ │ - Implemented helper functions in utils.rs               │  │
│ │ - Added comprehensive test suite in tests/feature.rs     │  │
│ │                                                          │  │
│ │ **Working Memory**:                                      │  │
│ │ - Project structure: src/, tests/, config/               │  │
│ │ - Key files: feature.rs, config.rs, utils.rs            │  │
│ │ - Dependencies: serde, tokio, anyhow                     │  │
│ │                                                          │  │
│ │ **Next Steps**:                                          │  │
│ │ - Add documentation as requested                         │  │
│ │ - Consider error handling improvements                   │  │
│ └─────────────────────────────────────────────────────────┘  │
└───────────────────────────────────────────────────────────────┘
```

### Session Instance Behavior

When you open a session that has compaction records:

1. **Full History Loaded**: All messages from the beginning are loaded from persistence
2. **Compaction Markers Rendered**: UI inserts visual markers at each compaction point
3. **Scrolling**: User can scroll through all messages, including archived ones
4. **Agent Behavior**: When agent starts, it only sees messages after the last compaction point
5. **Archived Messages**: Shown in a slightly dimmed style or with an "archived" indicator (optional)

### Data Flow

```
Persistence (ChatSession)
├── messages: [
│   msg1,
│   msg2,
│   ...,
│   msg44,
│   msg45: {  // Compaction message
│       role: "user",
│       content: [
│           ContextCompaction {
│               compaction_number: 1,
│               summary: "...",
│               messages_archived: 44,
│               ...
│           },
│           Text { text: "Continue based on summary..." }
│       ]
│   },
│   msg46,
│   ...,
│   msg100
│]

↓ Load Session ↓

SessionInstance
├── session.messages = [msg1, msg2, ..., msg45, ..., msg100]  // Full history

↓ Display in UI ↓

ChatView renders:
- msg1 to msg44 (archived, shown with dimmed style)
- [Compaction Marker #1 - expandable] (rendered from msg45's ContextCompaction block)
- msg46 to msg100 (active messages)

↓ Agent Starts ↓

Agent.get_active_messages() returns:
- msg45 to msg100 only
  (messages starting from compaction message)

↓ Send to LLM ↓

LLM Request contains:
- msg45 (with compaction block) to msg100
  (NOT the full history)
```

## Implementation Steps

### Phase 1: Core Functionality (High Priority)

1. **Add context_limit to ModelConfig**
   - Modify `crates/llm/src/provider_config.rs` to add `context_limit` field
   - Update `models.example.json` with context limits for each model
   - Test configuration loading

2. **Add ContextCompaction ContentBlock**
   - Add `ContentBlock::ContextCompaction` variant to `crates/llm/src/types.rs`
   - Add helper method `new_context_compaction()`
   - Update ContentBlock methods if needed (timestamps, equality, etc.)
   - Test serialization/deserialization

3. **Add ContextWindowConfig to Agent**
   - Add `ContextWindowConfig` struct to Agent
   - Implement `should_request_summary()` method
   - Implement `get_current_context_size()` method
   - Implement `count_compactions()` method (scans messages for compaction blocks)
   - Implement `get_active_messages()` method (returns messages after last compaction)
   - Implement `get_compaction_data()` method (extracts compaction info for UI)

3. **Implement context checking**
   - Add check before LLM requests in `run_single_iteration()`
   - Test threshold detection logic

4. **Implement summary request**
   - Add `request_context_summary()` method
   - Add `generate_summary_request_message()` method
   - Add `extract_text_from_response()` method
   - Test summary extraction

5. **Implement context compaction (not deletion!)**
   - Add `compact_context_window()` method
   - Create message with `ContextCompaction` block
   - Add instructional text block
   - Append message to history (marks the boundary)
   - DO NOT delete any messages from history

6. **Modify message rendering for agent**
   - Update `render_tool_results_in_messages()` to only return active messages
   - Use `get_active_messages()` to filter message history
   - Ensure tool executions are properly filtered

### Phase 2: Persistence & Configuration (Medium Priority)

7. **Verify persistence works (no changes needed!)**
   - Test that `ContextCompaction` blocks are properly serialized/deserialized
   - Test loading sessions with compaction messages
   - Verify message history integrity

8. **Add configuration options**
   - Add `context_threshold` and `context_management_enabled` to `SessionConfig`
   - Add loading logic in Agent initialization
   - Add method to update context config when model changes
   - Test configuration

### Phase 3: UI Integration (Medium-High Priority)

9. **Add UI events**
   - Define `ContextCompacted` event in `ui_events.rs`
   - Emit event when compaction occurs
   - Optional: Add `ContextUsageUpdate` for progress indicator

10. **Create CompactionMarker UI element**
    - Create `CompactionMarker` struct/component in GPUI code
    - Implement expandable/collapsible view
    - Show compaction stats (messages archived, tokens freed)
    - Display summary when expanded
    - Style appropriately (distinct from regular messages)

11. **Integrate markers into ChatView**
    - Modify message rendering to insert compaction markers
    - Load compaction records from session
    - Place markers at correct message indices
    - Handle multiple compaction markers
    - Ensure proper scrolling behavior

12. **Handle compaction blocks in stream processor**
    - Update streaming processor to recognize `ContextCompaction` blocks
    - Convert to special `DisplayFragment` type or handle specially
    - Ensure compaction blocks are properly displayed during live streaming

13. **Session loading with compaction data**
    - Ensure `SessionInstance` can extract compaction data from messages
    - Add method to get compaction data for UI (or use agent's `get_compaction_data()`)
    - Pass compaction info to UI during session connect

### Phase 4: Polish and Testing (Low Priority)

14. **Optional: Style archived messages**
    - Add visual indicator for archived messages (dimmed, labeled, etc.)
    - Make it clear which messages are in active context vs archived
    - Could be done by checking if message index is before a compaction

15. **Optional: Context usage indicator**
    - Add progress bar or indicator showing context fill percentage
    - Update in real-time as messages are added
    - Warn when approaching threshold

16. **Unit tests**
    - Test context size calculation
    - Test threshold detection
    - Test message filtering (active vs all)
    - Test compaction block creation and detection
    - Test `count_compactions()` and `get_active_messages()`

17. **Integration tests**
    - Test full compaction flow with mock LLM
    - Test multiple compactions in sequence
    - Test session save/load with compaction messages
    - Test UI rendering with compaction markers
    - Test that compaction blocks serialize/deserialize correctly

18. **Manual testing**
    - Long coding task that triggers compaction
    - Verify UI shows all messages + markers
    - Verify agent only sees active messages
    - Test session reload and continuity
    - Test summary quality and continuation

### Phase 5: Documentation (Low Priority)

19. **Update documentation**
    - Document configuration options
    - Add examples to README
    - Document UI behavior
    - Document the `ContextCompaction` content block format
    - Document limitations and best practices

## Edge Cases & Considerations

1. **First message after compaction**: The preserved summary becomes the first user message in the new context segment, so the agent starts with a clean slate but remains informed.

2. **Tool executions**: Kept in storage but only those relevant to active messages are included in LLM requests. The summary should capture important tool results from archived messages.

3. **Working memory**: NOT cleared - file trees, available projects, and plan state are preserved across compactions.

4. **Multiple compactions**: Can happen multiple times in a long session. Each is tracked with its own `CompactionRecord`.

5. **Disabled context management**: If `enabled=false` or `limit=None`, no checks or compactions occur.

6. **Summary quality**: The quality of continuation depends entirely on the LLM's summary. The system message is designed to encourage comprehensive summaries.

7. **Token counting accuracy**: We rely on the provider's reported token counts. Minor discrepancies are acceptable.

8. **Mid-tool-execution**: We check BEFORE sending to LLM, so we never compact mid-response.

9. **Playback mode**: Context management should be disabled during playback to preserve recorded behavior.

10. **CLI vs GUI**: Feature works the same in both modes, but GPUI shows visual compaction markers while CLI might just log the event.

11. **UI message display**: The UI shows ALL messages (archived + active) along with compaction markers. Only the agent loop filters to active messages.

12. **Session loading**: When loading a session, compaction records are preserved and the UI reconstructs the full view including markers.

13. **Message indices**: Compaction records store the message index where compaction occurred. This is the boundary between archived and active messages.

14. **Tool execution visibility**: Archived tool executions are still visible in the UI for reference, but their results aren't re-sent to the LLM in subsequent requests.

## Configuration Examples

### Enable with custom threshold
```json
// SessionConfig
{
  "context_threshold": 0.80,  // Trigger at 80%
  "context_management_enabled": true
}
```

### Disable context management
```json
// SessionConfig
{
  "context_management_enabled": false
}
```

### Model without context limit
```json
// models.json
{
  "Some Model": {
    "provider": "some-provider",
    "id": "some-model"
    // No context_limit specified - context management won't activate
  }
}
```

## Testing Strategy

1. **Unit Tests**
   - Context size calculation
   - Threshold detection
   - Message preservation logic

2. **Integration Tests**
   - Full reset flow with mock LLM
   - Multiple resets in sequence
   - Configuration variations

3. **Manual Testing**
   - Long coding task that triggers reset
   - Verify continuation quality
   - Check persistence across sessions

## Why This Approach?

### Compared to "Just Delete Old Messages"

**Benefits of Archiving vs Deleting:**

1. **User Transparency**: Users can see the full conversation history and understand what happened
2. **Reference Material**: Users can scroll back to see exact details of earlier work
3. **Debugging**: When things go wrong, having full history helps diagnose issues
4. **Session Continuity**: Reopening a session shows complete context, not gaps
5. **Audit Trail**: For code review or compliance, full history is preserved

### Compared to "Smart Message Pruning"

**Benefits of Compaction vs Pruning:**

1. **Simplicity**: Clear boundary between archived and active context
2. **Predictability**: User knows exactly when and why compaction happened
3. **LLM-Driven**: The LLM itself decides what's important via the summary
4. **All-or-Nothing**: No complex heuristics about which messages to keep
5. **Explicit Markers**: UI shows exactly where compaction occurred

### Compared to "No Context Management"

**Benefits of Automatic Compaction:**

1. **Reliability**: Tasks don't fail when hitting context limits
2. **Cost Efficiency**: Reduced token costs for very long conversations
3. **Performance**: Faster LLM responses with smaller context
4. **Scalability**: Enables truly long-running tasks
5. **Graceful Degradation**: System stays functional under high context pressure

## Future Enhancements

1. **Smart message pruning**: Instead of clearing everything, keep recent messages and prune old ones.

2. **Multiple summary levels**: Request intermediate summaries before full reset.

3. **Context compression**: Use a separate LLM call to compress old messages.

4. **User notification**: Ask user before resetting (optional mode).

5. **Summary review**: Allow user to edit the summary before continuing.

6. **Per-model strategies**: Different context management strategies for different model types.

7. **Cache optimization**: Structure resets to maximize prompt caching benefits.
