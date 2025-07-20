# Pending Messages Feature - Implementation Plan

## Overview

This document outlines the implementation plan for the "pending messages" feature, which allows users to queue additional messages while the agent is actively running, and provides the ability to edit queued messages.

## Current Architecture Analysis

### Message Flow
1. User submits message via `RootView::on_submit_click()`
2. Message is sent as `UiEvent::SendUserMessage` to backend
3. Backend triggers `Agent::run_single_iteration()`
4. Agent processes message and streams response back to UI
5. During streaming, only cancel is available via button state change

### Key Components
- **Agent Runner** (`crates/code_assistant/src/agent/runner.rs`): Core agent loop logic
- **Root View** (`crates/code_assistant/src/ui/gpui/root.rs`): Main UI with send/cancel button
- **Messages View** (`crates/code_assistant/src/ui/gpui/messages.rs`): Message display
- **UI Events** (`crates/code_assistant/src/ui/ui_events.rs`): Event communication
- **Streaming State** (`crates/code_assistant/src/ui/mod.rs`): Current streaming states

## Feature Requirements

### Core Functionality
1. **Message Queuing**: Allow users to send messages while agent is running
2. **Single Pending Message**: Only one message can be pending at a time
3. **Message Appending**: New messages append to existing pending message
4. **Agent Integration**: Pending messages are added to message history before next LLM request
5. **UI Display**: Show pending messages below the currently streaming assistant message

### Advanced Features
1. **Message Editing**: Up arrow key moves pending message back to input for editing (when input is empty)
2. **Dual Buttons**: Separate send and cancel buttons with independent enable/disable logic

### UI Behavior
- **Send Button**: Enabled when text input has content
- **Cancel Button**: Enabled when session has running agent
- **Pending Message Display**: Shows below last assistant message during streaming
- **Message Streaming**: Chunks append to last *assistant* message, not last message overall

## Implementation Plan

### Phase 1: Backend State Management

#### 1.1 Extend Agent State
**File**: `crates/code_assistant/src/agent/runner.rs`

Add pending message state to Agent struct:
```rust
pub struct Agent {
    // ... existing fields
    pending_user_message: Arc<Mutex<Option<String>>>,
}
```

Add methods:
- `queue_user_message(message: String)` - Queue or append to pending message (thread-safe)
- `get_and_clear_pending_message() -> Option<String>` - Retrieve and clear pending message (thread-safe)
- `has_pending_message() -> bool` - Check if message is pending (thread-safe)

#### 1.2 Modify Agent Loop
**File**: `crates/code_assistant/src/agent/runner.rs`

Update `run_single_iteration_internal()`:
- Before calling LLM, check for pending message
- If pending message exists, add it to message history
- Clear pending message after adding to history

#### 1.3 Session State Management
**File**: `crates/code_assistant/src/session/mod.rs`

Update `SessionState` to include:
```rust
pub struct SessionState {
    // ... existing fields
    pub pending_user_message: Option<String>,
}
```

### Phase 2: UI Events and Communication

#### 2.1 New UI Events
**File**: `crates/code_assistant/src/ui/ui_events.rs`

Add new events:
```rust
pub enum UiEvent {
    // ... existing events
    /// Queue a user message while agent is running
    QueueUserMessage { message: String, session_id: String },
    /// Request to edit pending message (move back to input)
    RequestPendingMessageEdit { session_id: String },
    /// Update pending message display
    UpdatePendingMessage { message: Option<String> },
}
```

#### 2.2 Backend Event Handling
**File**: `crates/code_assistant/src/ui/gpui/mod.rs`

Add handling for new events in `process_ui_event_async()`:
- `QueueUserMessage`: Forward to backend
- `RequestPendingMessageEdit`: Request pending message from backend
- `UpdatePendingMessage`: Update UI display

### Phase 3: UI State Management

#### 3.1 Use Existing Session Activity State
**File**: `crates/code_assistant/src/session/instance.rs`

The UI already tracks agent state via `SessionActivityState` which includes:
- `Idle`: Agent not running
- `WaitingForResponse`: Agent waiting for LLM
- `RateLimited`: Agent rate limited
- `Running`: Agent executing tools

Use existing `SessionActivityState` to determine if agent is running, no new state tracking needed.

#### 3.2 Root View Updates
**File**: `crates/code_assistant/src/ui/gpui/root.rs`

Major changes to `RootView`:

**Button Logic**:
- Split single send/cancel button into two separate buttons
- Send button: enabled when input has content
- Cancel button: enabled when `SessionActivityState` indicates agent is running
- Update button rendering logic in `render()` to use existing session activity state
- No additional state fields needed in RootView

**Input Handling**:
- Modify `on_submit_click()` to check agent state
- If agent running: queue message via `QueueUserMessage`
- If agent idle: send normal message
- Add keyboard handler for up arrow key

#### 3.3 Message Display Updates
**File**: `crates/code_assistant/src/ui/gpui/messages.rs`

Update `MessagesView` to:
- Display pending messages below last assistant message
- Handle pending message styling/formatting
- Ensure streaming chunks append to correct message

### Phase 4: Streaming Integration

#### 4.1 Message Container Updates
**File**: `crates/code_assistant/src/ui/gpui/elements.rs`

Update `MessageContainer` to:
- Track whether it's the "active" assistant message for streaming
- Ensure chunks append to correct container
- Handle pending message display

#### 4.2 Streaming Logic
**File**: `crates/code_assistant/src/ui/gpui/mod.rs`

Update streaming event handling:
- `AppendToTextBlock`: Ensure appends to last *assistant* message
- Add logic to differentiate between assistant messages and pending user messages

### Phase 5: Session Management Integration

#### 5.1 Session Instance Updates
**File**: `crates/code_assistant/src/session/instance.rs` (inferred)

Update session management to:
- Handle queued message events
- Coordinate between UI and agent for pending messages
- Manage agent running state

#### 5.2 Persistence Updates
Update persistence layer to save/restore pending message state with sessions.

### Phase 6: Advanced Features

#### 6.1 Message Editing
**File**: `crates/code_assistant/src/ui/gpui/root.rs`

Implement up arrow functionality:
- Detect up arrow keypress
- Check if input is empty
- Request pending message from backend
- Move pending message to input field
- Clear pending message from backend

#### 6.2 Message Appending Logic
**File**: `crates/code_assistant/src/agent/runner.rs`

Implement smart message appending:
- When new message queued and one exists, append with appropriate separator
- Handle edge cases (empty messages, whitespace, etc.)

## Technical Considerations

### Concurrency
- Agent pending message state uses Arc<Mutex<Option<String>>> for thread safety
- UI leverages existing SessionActivityState for agent status
- Event ordering maintained through existing async channel system

### Error Handling
- Handle edge cases where agent stops unexpectedly with pending messages
- Graceful degradation if pending message operations fail
- UI feedback for error states

### Performance
- Minimal impact on existing streaming performance
- Efficient pending message state management
- Avoid unnecessary UI re-renders

### User Experience
- Clear visual distinction between streaming and pending messages
- Intuitive button states and feedback
- Consistent behavior across different scenarios

## Testing Strategy

### Unit Tests
- Agent pending message queue operations
- UI event handling for new events
- Button state logic

### Integration Tests
- End-to-end message queueing flow
- Agent loop with pending messages
- UI state coordination

### Manual Testing
- Various user interaction patterns
- Edge cases and error conditions
- Performance under load

## Implementation Strategy

### Direct Implementation
- Modify existing code paths directly rather than creating parallel implementations
- Update session state structure to include pending messages
- Change existing UI components to support the new functionality
- Break existing code temporarily during implementation phases for cleaner final result

## Risks and Mitigations

### Technical Risks
- **Complexity**: Feature spans multiple components
  - *Mitigation*: Direct modification of existing code paths, phased implementation
- **Race Conditions**: Multiple async operations with shared state
  - *Mitigation*: Use Arc<Mutex<T>> for shared agent state, careful event ordering
- **State Consistency**: UI and backend state synchronization
  - *Mitigation*: Leverage existing SessionActivityState for agent status

## Success Criteria

### Functional
- Users can queue messages while agent is running
- Queued messages are properly processed by agent
- Message editing works as specified
- Button states correctly reflect system state

### Non-Functional
- No regression in existing functionality
- Minimal performance impact
- Stable under various usage patterns
- Intuitive user experience

## Implementation Phases

The feature will be implemented in phases, with each phase building on the previous one. Temporary breakage between phases is acceptable for cleaner final implementation.

This is a complex feature that requires careful coordination between multiple components. The phased approach allows for incremental progress while directly modifying existing code paths for cleaner implementation.
---
