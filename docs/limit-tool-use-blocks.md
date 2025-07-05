# Limiting Tool Use Blocks Implementation Plan

## Problem Analysis

### Issue
The LLM sometimes generates multiple tool blocks in a single message, despite being instructed to output only one tool per turn. This is inefficient and problematic because:

- The output of the first tool would inform the next action
- No tool output can be provided until the LLM ends its turn
- Multiple tools per message are usually less efficient or incorrect

### Current Behavior (Problematic)
```
<tool:read_files>
<param:path>file.txt</param:path>
</tool:read_files>

<tool:replace_in_file>  ← Second tool starts here
<param:path>file.txt</param:path>
...
```

### Desired Behavior
Stop generation after the first complete tool block, treating it as a normal successful response.

## Approaches Considered

### ❌ Option 1: Stop Sequences (Attempted)
- **Approach**: Use API stop sequences with tool closing tags like `</tool:read_files>`
- **Problem**: Stop sequences cause generation to stop when encountered, but the stop sequence itself is not included in the streamed response
- **Result**: Incomplete tool blocks (missing closing tags)
- **Conclusion**: Doesn't work as expected

### ❌ Option 2: Custom Stop Markers
- **Approach**: Modify system prompt to add markers like `[TOOL_COMPLETE]` after each tool
- **Problem**: If LLM already ignores "one tool per turn" instruction, why would it follow a new instruction more reliably?
- **Conclusion**: Adds complexity without solving the reliability issue

### ✅ Option 3: Streaming Processor Detection (Selected)
- **Approach**: Detect second tool start in XML streaming processor and handle gracefully
- **Benefits**: 
  - Deterministic fail-safe mechanism
  - Code-based detection vs instruction-based prevention
  - Works regardless of LLM instruction compliance
  - Preserves first tool, discards incomplete second one

## Implementation Architecture

### Key Insight
The error handling must occur at the **LLM Provider level**, not the Agent level:

- **XML Streaming Processor**: Detects second tool and returns special error
- **LLM Provider**: Catches error, stops processing, returns successful response with first tool
- **Agent**: Receives normal `LLMResponse`, processes first tool normally
- **No error feedback**: Conversation continues seamlessly

### Error Flow Comparison

#### Current (User Cancellation)
```
StreamingCallback -> Error -> Agent -> Discard entire message
```

#### New (Tool Limit)
```
StreamingCallback -> Special Error -> LLM Provider -> Early Success Return
                                                   -> Agent -> Process first tool
```

## Implementation Plan

### 1. Create New Error Type
**File**: `crates/llm/src/types.rs` or streaming-specific module

```rust
#[derive(Debug, thiserror::Error)]
pub enum StreamingError {
    #[error("Tool limit reached - only one tool per message allowed")]
    ToolLimitReached,
    #[error("Streaming cancelled by user")]
    UserCancelled,
    // ... other streaming errors
}
```

### 2. Modify XML Streaming Processor
**File**: `crates/code_assistant/src/ui/streaming/xml_processor.rs`

**Detection Logic**:
- Track `tool_counter` in processor state
- When detecting `TagType::ToolStart`:
  - If `tool_counter >= 1`: Return `StreamingError::ToolLimitReached`
  - Else: Continue normally

**Key Points**:
- Only detect at tool start (not tool end)
- Return error immediately when second tool begins
- First tool should be complete at this point

### 3. Update LLM Providers
**Files**: 
- `crates/llm/src/anthropic.rs`
- `crates/llm/src/aicore_invoke.rs`

**In streaming loop** (around the `process_chunk`/`process_sse_line` functions):

```rust
match callback(chunk) {
    Ok(()) => continue,
    Err(e) if e.to_string().contains("Tool limit reached") => {
        // Stop processing chunks, return response collected so far
        debug!("Tool limit reached, stopping streaming early");
        break; // Exit chunk processing loop
    },
    Err(e) if e.to_string().contains("Streaming cancelled by user") => {
        // Existing cancellation handling
        return Err(e);
    },
    Err(e) => return Err(e), // Other errors
}
```

**Result**: Return normal `LLMResponse` with blocks collected up to the tool limit.

### 4. Update Streaming Callback Type
**File**: `crates/llm/src/lib.rs` (or wherever `StreamingCallback` is defined)

Current:
```rust
type StreamingCallback = Box<dyn Fn(&StreamingChunk) -> Result<(), anyhow::Error>>;
```

Potentially update to use the new error type for better error handling.

### 5. Testing Strategy

#### Unit Tests
- **XML Processor**: Test detection of second tool start
- **Mock Streaming**: Simulate tool limit scenarios

#### Integration Tests  
- **End-to-end**: LLM generates multiple tools, verify only first is processed
- **Tool Execution**: Ensure first tool executes normally
- **Conversation Flow**: Verify conversation continues after tool limit

#### Test Cases
1. **Single tool**: Normal behavior unchanged
2. **Two complete tools**: Second tool detected and stopped
3. **Incomplete second tool**: Detection works even with partial tool tags
4. **Tool limit with parameters**: Ensure parameter state is properly reset

## Technical Details

### XML Processor State Tracking
- `tool_counter`: Track number of tools started in current message
- `in_tool`: Existing flag for current tool state
- **Detection Point**: `TagType::ToolStart` processing

### Error Message Format
Use consistent error message format that LLM providers can reliably detect:
```
"Tool limit reached - only one tool per message allowed"
```

### Response Preservation
When tool limit reached:
- **Keep**: All `ContentBlock`s collected so far (should include complete first tool)
- **Keep**: Usage statistics accumulated
- **Keep**: Rate limit information
- **Discard**: Any partial second tool content

### Logging
Add debug logging for troubleshooting:
```rust
debug!("Tool limit reached at tool #{}, stopping generation", tool_counter);
debug!("Collected {} content blocks before tool limit", blocks.len());
```

## Benefits

1. **Reliability**: Code-based constraint enforcement
2. **Performance**: Stops unnecessary token generation early
3. **User Experience**: Seamless - no visible errors or disruption
4. **Maintainability**: Centralized constraint logic in XML processor
5. **Flexibility**: Can be disabled/modified without changing LLM instructions

## Implementation Order

1. Create error type and update streaming callback signatures
2. Implement detection logic in XML streaming processor
3. Update Anthropic provider to handle tool limit gracefully
4. Update AI Core provider to handle tool limit gracefully  
5. Add comprehensive testing
6. Verify behavior with real LLM interactions

## Notes

- This solution works only for XML mode (where XML processor is used)
- Native mode uses LLM provider's built-in tool calling and shouldn't have this issue
- The approach provides a deterministic safety net regardless of LLM instruction compliance
- No changes needed to agent logic or conversation flow