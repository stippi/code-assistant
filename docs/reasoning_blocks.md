# OpenAI Reasoning Block Implementation Plan

## Context

We are implementing support for OpenAI's Responses API reasoning features in our LLM provider system. The OpenAI reasoning output is more fine-grained than traditional "thinking" content: while the LLM is reasoning, it outputs separate "summary items" with titles, and the actual reasoning content is hidden/opaque as "encrypted_content".

### Current Architecture

- **LLM Providers** return completed `ContentBlock`s from their `send_message()` method
- **Streaming** happens via `StreamingCallback` which routes through Streaming Processors (XML, Caret, JSON)
- **Streaming Processors** convert chunks to `DisplayFragment`s for UI display
- **UI Components** display both historical messages (converted from ContentBlocks to DisplayFragments) and live streaming (as DisplayFragments)
- **MessageContainer** manages blocks in the GPUI implementation with expand/collapse functionality

### Current Issue

The current `ToolUseBlock` incorrectly mixes generating state with collapsed/expanded state via `ToolBlockState` enum. We need to separate these concerns for all block types.

## Goal

Implement OpenAI reasoning blocks with the following UX:

**While generating:**
- Collapsed: Show current summary item title
- Expanded: Show current summary item title + content

**When completed:**
- Collapsed: Show "Thought for X seconds"
- Expanded: Show all summary items formatted as markdown

**Key principle:** All block types should have separate generating state and expand/collapse state.

## Implementation Plan

### Phase 1: Fix Block State Architecture

**File: `crates/code_assistant/src/ui/gpui/elements.rs`**

1. **Add universal generating state tracking:**
   ```rust
   pub struct BlockView {
       block: BlockData,
       request_id: u64,
       is_generating: bool, // NEW: Universal generating state
       // ... existing fields
   }
   ```

2. **Fix `ToolBlockState` enum:**
   ```rust
   #[derive(Debug, Clone, PartialEq)]
   pub enum ToolBlockState {
       Collapsed,
       Expanded,
       // Remove Generating - this becomes is_generating: bool
   }
   ```

3. **Update `ToolUseBlock`:**
   ```rust
   pub struct ToolUseBlock {
       // ... existing fields
       pub state: ToolBlockState, // Only collapsed/expanded
       // Remove completed field - use BlockView.is_generating instead
   }
   ```

4. **Add methods to `BlockView`:**
   ```rust
   impl BlockView {
       pub fn set_generating(&mut self, generating: bool);
       pub fn is_generating(&self) -> bool;
       pub fn can_toggle_expansion(&self) -> bool; // Can't expand while generating for some block types
   }
   ```

### Phase 2: Enhance Types for OpenAI Reasoning

**File: `crates/llm/src/types.rs`**

1. **Add `ReasoningSummaryItem` struct:**
   ```rust
   #[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
   pub struct ReasoningSummaryItem {
       pub title: String,
       pub content: Option<String>,
   }
   ```

2. **Update `RedactedThinking` ContentBlock:**
   ```rust
   #[serde(rename = "redacted_thinking")]
   RedactedThinking {
       id: String,
       summary: Vec<serde_json::Value>, // Keep for backward compatibility
       summary_items: Vec<ReasoningSummaryItem>, // NEW: Structured summary items
       data: String,
       #[serde(skip_serializing_if = "Option::is_none")]
       start_time: Option<SystemTime>,
       #[serde(skip_serializing_if = "Option::is_none")]
       end_time: Option<SystemTime>,
   },
   ```

**File: `crates/llm/src/lib.rs`**

3. **Add new streaming chunk types:**
   ```rust
   pub enum StreamingChunk {
       // ... existing variants
       /// OpenAI reasoning summary item delta with ID for tracking
       ReasoningSummary { id: String, delta: String },
       /// Indicates reasoning block is complete
       ReasoningComplete,
   }
   ```

### Phase 3: Update OpenAI Responses Provider

**File: `crates/llm/src/openai_responses.rs`**

1. **Add reasoning state tracking:**
   ```rust
   struct ReasoningState {
       current_item_title: Option<String>,
       current_item_content: String,
       completed_items: Vec<ReasoningSummaryItem>,
   }
   ```

2. **Update `process_sse_line` method:**
   - Track reasoning state during streaming
   - Handle `response.reasoning_summary_text.delta` events
   - Emit `StreamingChunk::ReasoningSummary` with item ID and delta content
   - Emit `StreamingChunk::ReasoningComplete` when reasoning finishes
   - Use item_id from SSE events to track different summary items

3. **Update `convert_output` method:**
   - Convert completed reasoning to `RedactedThinking` with both `summary` and `summary_items`
   - Populate `summary_items` from collected `ReasoningSummaryItem` structs

### Phase 4: Enhance DisplayFragment Types

**File: `crates/code_assistant/src/ui/streaming/mod.rs`**

1. **Add new `DisplayFragment` variants:**
   ```rust
   pub enum DisplayFragment {
       // ... existing variants
       /// OpenAI reasoning summary item delta with ID for tracking
       ReasoningSummary { id: String, delta: String },
       /// Mark reasoning as completed
       ReasoningComplete,
   }
   ```

2. **Update all stream processors** (XML, Caret, JSON) to handle:
   - `StreamingChunk::ReasoningSummary` → `DisplayFragment::ReasoningSummary` (pass through)
   - `StreamingChunk::ReasoningComplete` → `DisplayFragment::ReasoningComplete`
   - UI layer will handle parsing title from content and detecting new items by ID changes

### Phase 5: Update UI Components for Reasoning Display

**File: `crates/code_assistant/src/ui/gpui/elements.rs`**

1. **Enhance `ThinkingBlock` for reasoning state:**
   ```rust
   pub struct ThinkingBlock {
       pub content: String,
       pub is_collapsed: bool,
       pub is_completed: bool, // Keep for backward compatibility with traditional thinking
       pub start_time: std::time::Instant,
       pub end_time: std::time::Instant,
       // NEW: OpenAI reasoning fields
       pub reasoning_summary_items: Vec<ReasoningSummaryItem>,
       pub current_generating_title: Option<String>,
       pub current_generating_content: Option<String>,
   }
   ```

2. **Add reasoning-specific methods:**
   ```rust
   impl ThinkingBlock {
       pub fn update_reasoning_summary(&mut self, id: String, delta: String);
       pub fn complete_reasoning(&mut self);
       pub fn get_display_title(&self, is_generating: bool) -> String;
       pub fn get_expanded_content(&self, is_generating: bool) -> String;
       pub fn is_reasoning_block(&self) -> bool; // Has reasoning_summary_items
       fn parse_title_from_content(content: &str) -> Option<String>; // Parse "**title**:" format
   }
   ```

3. **Update thinking block rendering logic:**
   - Check `BlockView.is_generating` to determine display mode
   - Use reasoning-specific display methods when `is_reasoning_block()` returns true
   - Fall back to traditional thinking display for non-reasoning blocks

### Phase 6: Update MessageContainer for Reasoning Events

**File: `crates/code_assistant/src/ui/gpui/elements.rs`**

1. **Add reasoning methods to `MessageContainer`:**
   ```rust
   impl MessageContainer {
       pub fn update_reasoning_summary(&self, id: String, delta: String, cx: &mut Context<Self>);
       pub fn complete_reasoning(&self, cx: &mut Context<Self>);
       pub fn set_block_generating(&self, generating: bool, cx: &mut Context<Self>);
   }
   ```

2. **Update existing methods to use universal generating state:**
   - `update_tool_status` should call `set_block_generating(false, cx)` when tool completes
   - `finish_any_thinking_blocks` should call `set_block_generating(false, cx)`

### Phase 7: Update GPUI Event Handling

**File: `crates/code_assistant/src/ui/gpui/mod.rs`**

1. **Add new `UiEvent` variants:**
   ```rust
   pub enum UiEvent {
       // ... existing variants
       UpdateReasoningSummary { id: String, delta: String },
       CompleteReasoning,
   }
   ```

2. **Update `display_fragment` method:**
   ```rust
   fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError> {
       match fragment {
           // ... existing cases
           DisplayFragment::ReasoningSummary { id, delta } => {
               self.push_event(UiEvent::UpdateReasoningSummary {
                   id: id.clone(),
                   delta: delta.clone(),
               });
           }
           DisplayFragment::ReasoningComplete => {
               self.push_event(UiEvent::CompleteReasoning);
           }
       }
       Ok(())
   }
   ```

3. **Update `process_ui_event_async`:**
   - Handle `UpdateReasoningSummary` events (with ID and delta parsing)
   - Handle `CompleteReasoning` events
   - Update existing events to use universal generating state
   - UI layer detects new items by ID changes and parses titles from content

### Phase 8: Fragment Extraction for Session History

**Files: All streaming processors in `crates/code_assistant/src/ui/streaming/`**

1. **Update `extract_fragments_from_message` methods:**
   - Handle `RedactedThinking` blocks with `summary_items`
   - Generate `DisplayFragment::ReasoningSummary` for each summary item (synthesize ID and full content)
   - End with `DisplayFragment::ReasoningComplete`
   - Use ContentBlock timestamps for proper timing

2. **Ensure consistent fragment ordering:**
   - Traditional thinking: `ThinkingText` → fragments
   - OpenAI reasoning: `ReasoningSummary` (multiple) → `ReasoningComplete`

## Implementation Order

1. **✅ Phase 1**: Fix universal block state architecture (critical foundation) - COMPLETED
2. **✅ Phase 2**: Core type changes for reasoning support - COMPLETED
3. **✅ Phase 3**: OpenAI provider streaming updates - COMPLETED
4. **✅ Phase 4**: DisplayFragment enhancements - COMPLETED
5. **✅ Phase 5**: UI component reasoning display logic - COMPLETED
6. **✅ Phase 6**: MessageContainer integration - COMPLETED
7. **✅ Phase 7**: Event system updates - COMPLETED
8. **✅ Phase 8**: Session history fragment extraction - COMPLETED

## ✅ IMPLEMENTATION COMPLETE

All phases have been successfully implemented. The OpenAI Responses API reasoning features are now fully integrated into the codebase.

## Key Architecture Principles

1. **Separation of concerns**: Generating state is separate from expand/collapse state for all blocks
2. **Universal state management**: All block types use the same generating state tracking
3. **Fragment consistency**: Both live streaming and session history use DisplayFragments
4. **Backward compatibility**: Traditional thinking blocks continue working unchanged
5. **Progressive enhancement**: OpenAI reasoning is additive to existing functionality

## Testing Strategy

- Test traditional thinking blocks still work with new architecture
- Test tool blocks work correctly with separated state
- Test OpenAI reasoning blocks in both generating and completed states
- Test session loading with mixed block types
- Test expand/collapse behavior during and after generation

## Migration Notes

- Existing `ToolBlockState::Generating` usage needs to be converted to `BlockView.is_generating = true`
- Existing `completed` field in `ToolUseBlock` should be replaced with `BlockView.is_generating = false`
- All block rendering logic needs to check `BlockView.is_generating` instead of block-specific state
