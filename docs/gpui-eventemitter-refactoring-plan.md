# GPUI EventEmitter Refactoring Implementation Plan

## Current State Analysis

After analyzing the UI code in `crates/code_assistant/src/ui/gpui/`, the current communication patterns are:

### Current Communication Patterns

1. **Channel-based Communication**: Heavy use of `async_channel` for UI ↔ Backend communication
2. **Direct Method Calls**: Components directly call methods on other components via `Entity<T>`
3. **Shared State**: Extensive use of `Arc<Mutex<T>>` for shared state between components
4. **Single EventEmitter**: Only `AttachmentView` implements `EventEmitter<AttachmentEvent>`
5. **Global Event System**: Uses `UiEventSender` as a global for sending `UiEvent`s

### Problems with Current Architecture

1. **Tight Coupling**: Components are tightly coupled through direct method calls
2. **Complex State Management**: Extensive use of `Arc<Mutex<T>>` creates potential deadlocks
3. **Inconsistent Communication**: Mix of channels, direct calls, and single event emitter
4. **Hard to Test**: Tightly coupled components are difficult to unit test
5. **Race Conditions**: Shared mutable state without proper synchronization patterns

## Refactoring Strategy

### Phase 1: Component-Level EventEmitters (Low Risk)
Focus on internal component communication that doesn't affect UI ↔ Backend communication.

### Phase 2: UI Component Communication (Medium Risk)
Replace direct method calls between UI components with EventEmitter patterns.

### Phase 3: Unified Event Architecture (High Risk)
Consolidate the global event system with component-level events.

---

## Phase 1: Component-Level EventEmitters

### 1.1 Text Input Component Events ✓ COMPLETED

**Previous**: Direct subscription to `InputEvent` from `gpui_component`
**Current**: Custom `InputArea` component with `InputAreaEvent` enum

#### Completed Implementation:

1. **Custom Input Events Implemented**
```rust
// In crates/code_assistant/src/ui/gpui/input_area.rs
#[derive(Clone, Debug)]
pub enum InputAreaEvent {
    MessageSubmitted {
        content: String,
        attachments: Vec<DraftAttachment>,
    },
    ContentChanged {
        content: String,
        attachments: Vec<DraftAttachment>,
    },
    FocusRequested,
    CancelRequested,
    ClearDraftRequested,
}
```

2. **InputArea Component Created**
```rust
pub struct InputArea {
    text_input: Entity<InputState>,
    attachments: Vec<DraftAttachment>,
    attachment_views: Vec<Entity<AttachmentView>>,
    agent_is_running: bool,
    cancel_enabled: bool,
    _input_subscription: Subscription,
}

impl EventEmitter<InputAreaEvent> for InputArea {}
```

3. **RootView Updated to EventEmitter Pattern**
```rust
// Clean event subscription in RootView
let input_area_subscription = cx.subscribe_in(&input_area, window, Self::on_input_area_event);

fn on_input_area_event(&mut self, event: &InputAreaEvent, cx: &mut Context<Self>) {
    match event {
        InputAreaEvent::MessageSubmitted { content, attachments } => {
            // Handle message submission
        }
        InputAreaEvent::ContentChanged { content, attachments } => {
            // Handle draft saving
        }
        InputAreaEvent::CancelRequested => {
            // Handle agent cancellation
        }
        InputAreaEvent::ClearDraftRequested => {
            // Clear draft synchronously before input clearing
        }
    }
}
```

**Files Modified:**
- ✓ `crates/code_assistant/src/ui/gpui/root.rs` - Updated to use InputArea component
- ✓ `crates/code_assistant/src/ui/gpui/mod.rs` - Added input_area module, updated RootView constructor
- ✓ Created `crates/code_assistant/src/ui/gpui/input_area.rs` - Complete self-contained component

**Key Achievements:**
- Complete separation of input/attachment logic from RootView
- Event-driven communication with clear boundaries
- Maintained backward compatibility with V1 mode
- Fixed draft clearing race condition
- Improved button layout and state management

### 1.2 Chat Sidebar Component Events

**Current**: Direct method calls for session management
**Target**: EventEmitter for session selection and management

#### Implementation Steps:

1. **Define Chat Sidebar Events**
```rust
// In crates/code_assistant/src/ui/gpui/chat_events.rs
#[derive(Clone, Debug)]
pub enum ChatSidebarEvent {
    SessionSelected {
        session_id: String,
    },
    SessionDeleteRequested {
        session_id: String,
    },
    NewSessionRequested {
        name: Option<String>,
    },
    SessionsRefreshRequested,
}
```

2. **Update ChatSidebar to emit events**
```rust
impl ChatSidebar {
    fn handle_session_click(&mut self, session_id: String, cx: &mut Context<Self>) {
        cx.emit(ChatSidebarEvent::SessionSelected { session_id });
    }
}

impl EventEmitter<ChatSidebarEvent> for ChatSidebar {}
```

3. **Update RootView to subscribe to chat events**
```rust
let chat_subscription = cx.subscribe(&chat_sidebar, |this, _, event: &ChatSidebarEvent, cx| {
    match event {
        ChatSidebarEvent::SessionSelected { session_id } => {
            this.handle_session_selection(session_id.clone(), cx);
        }
        // ... other events
    }
});
```

**Files to modify:**
- `crates/code_assistant/src/ui/gpui/chat_sidebar.rs` (add EventEmitter implementation)
- `crates/code_assistant/src/ui/gpui/root.rs` (add chat sidebar subscription)
- Create `crates/code_assistant/src/ui/gpui/chat_events.rs`

### 1.3 Messages View Component Events

**Current**: Direct method calls for message management
**Target**: EventEmitter for message interactions

#### Implementation Steps:

1. **Define Messages View Events**
```rust
// In crates/code_assistant/src/ui/gpui/message_events.rs
#[derive(Clone, Debug)]
pub enum MessagesViewEvent {
    ScrolledToBottom,
    ScrolledToTop,
    MessageInteraction {
        message_id: String,
        interaction_type: MessageInteractionType,
    },
    PendingMessageEditRequested {
        session_id: String,
    },
}

#[derive(Clone, Debug)]
pub enum MessageInteractionType {
    Copy,
    Regenerate,
    Edit,
}
```

2. **Update MessagesView to emit events**
```rust
impl EventEmitter<MessagesViewEvent> for MessagesView {}
```

**Files to modify:**
- `crates/code_assistant/src/ui/gpui/messages.rs` (add EventEmitter implementation)
- Create `crates/code_assistant/src/ui/gpui/message_events.rs`

---

## Phase 2: UI Component Communication

### 2.1 Replace Arc<Mutex<T>> with EventEmitter Communication

**Current**: Shared state via `Arc<Mutex<Vec<Entity<MessageContainer>>>>`
**Target**: EventEmitter-based message updates

#### Implementation Steps:

1. **Create Message Management Events**
```rust
// In crates/code_assistant/src/ui/gpui/message_management.rs
#[derive(Clone, Debug)]
pub enum MessageManagementEvent {
    MessageAdded {
        message: Entity<MessageContainer>,
    },
    MessagesCleared,
    MessageUpdated {
        message_id: String,
    },
    StreamingStarted {
        request_id: u64,
    },
    StreamingStopped {
        request_id: u64,
        cancelled: bool,
    },
}
```

2. **Create MessageManager Component**
```rust
pub struct MessageManager {
    messages: Vec<Entity<MessageContainer>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<MessageManagementEvent> for MessageManager {}
```

3. **Update Components to Subscribe**
```rust
// In MessagesView
let message_subscription = cx.subscribe(&message_manager, |this, _, event: &MessageManagementEvent, cx| {
    match event {
        MessageManagementEvent::MessageAdded { message } => {
            this.handle_new_message(message.clone(), cx);
        }
        // ... other events
    }
});
```

**Files to modify:**
- `crates/code_assistant/src/ui/gpui/mod.rs` (remove Arc<Mutex> message_queue)
- `crates/code_assistant/src/ui/gpui/messages.rs` (subscribe to MessageManager)
- Create `crates/code_assistant/src/ui/gpui/message_manager.rs`

### 2.2 Memory View Integration

**Current**: Direct shared state `Arc<Mutex<Option<WorkingMemory>>>`
**Target**: EventEmitter for memory updates

#### Implementation Steps:

1. **Define Memory Events**
```rust
#[derive(Clone, Debug)]
pub enum MemoryEvent {
    MemoryUpdated {
        memory: WorkingMemory,
    },
    MemoryCleared,
    MemoryToggleRequested,
}
```

2. **Update MemoryView**
```rust
impl EventEmitter<MemoryEvent> for MemoryView {}
```

**Files to modify:**
- `crates/code_assistant/src/ui/gpui/memory.rs` (add EventEmitter)
- `crates/code_assistant/src/ui/gpui/root.rs` (subscribe to memory events)

---

## Phase 3: Unified Event Architecture

### 3.1 Create Central Event Hub

**Current**: Global `UiEventSender` with `UiEvent` enum
**Target**: Hierarchical event system with typed events

#### Implementation Steps:

1. **Create Event Hub Component**
```rust
// In crates/code_assistant/src/ui/gpui/event_hub.rs
pub struct EventHub {
    backend_sender: Option<async_channel::Sender<BackendEvent>>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone, Debug)]
pub enum HubEvent {
    BackendCommunication(BackendEvent),
    UIStateChange(UIStateEvent),
    ComponentInteraction(ComponentEvent),
}

impl EventEmitter<HubEvent> for EventHub {}
```

2. **Create Event Routing**
```rust
impl EventHub {
    fn route_event(&mut self, event: HubEvent, cx: &mut Context<Self>) {
        match event {
            HubEvent::BackendCommunication(backend_event) => {
                self.send_to_backend(backend_event);
            }
            HubEvent::UIStateChange(ui_event) => {
                cx.emit(HubEvent::UIStateChange(ui_event));
            }
            HubEvent::ComponentInteraction(component_event) => {
                self.handle_component_interaction(component_event, cx);
            }
        }
    }
}
```

**Files to modify:**
- `crates/code_assistant/src/ui/gpui/mod.rs` (replace global UiEventSender)
- Create `crates/code_assistant/src/ui/gpui/event_hub.rs`

### 3.2 Migrate from UiEvent to Typed Events

**Current**: Single `UiEvent` enum for everything
**Target**: Specific event types for different concerns

#### Implementation Steps:

1. **Create Specific Event Types**
```rust
// Backend communication events
#[derive(Clone, Debug)]
pub enum BackendCommunicationEvent {
    SessionLoad(String),
    SessionCreate(Option<String>),
    MessageSend { session_id: String, content: String, attachments: Vec<DraftAttachment> },
}

// UI state events
#[derive(Clone, Debug)]
pub enum UIStateEvent {
    MemoryToggled,
    SidebarToggled,
    ThemeChanged,
}

// Component interaction events
#[derive(Clone, Debug)]
pub enum ComponentInteractionEvent {
    InputSubmitted(TextInputEvent),
    ChatInteraction(ChatSidebarEvent),
    MessageInteraction(MessagesViewEvent),
}
```

2. **Create Event Translation Layer**
```rust
impl From<TextInputEvent> for ComponentInteractionEvent {
    fn from(event: TextInputEvent) -> Self {
        ComponentInteractionEvent::InputSubmitted(event)
    }
}
```

**Files to modify:**
- `crates/code_assistant/src/ui/ui_events.rs` (refactor UiEvent enum)
- All components that currently use UiEvent

---

## Implementation Timeline

### Week 1: Phase 1.1 - Text Input Events ✓ COMPLETED
- [x] Create `InputAreaEvent` enum and `InputArea` component
- [x] Update `RootView` to use EventEmitter pattern for text input
- [x] Test input functionality works correctly
- [x] Update attachment handling to use events
- [x] Fix draft clearing race condition with `ClearDraftRequested` event
- [x] Implement always-visible cancel button with proper state management

**Completed Implementation:**
- Created `crates/code_assistant/src/ui/gpui/input_area.rs` with `InputArea` component
- Implemented `EventEmitter<InputAreaEvent>` with events:
  - `MessageSubmitted` - when user submits message with content and attachments
  - `ContentChanged` - for draft saving functionality
  - `FocusRequested` - when input requests focus
  - `CancelRequested` - for agent cancellation
  - `ClearDraftRequested` - for synchronous draft clearing before input clear
- Updated `RootView` to subscribe to `InputAreaEvent` instead of direct input manipulation
- Removed coupling between `RootView` and input/attachment management
- Maintained V1 mode compatibility for `UserInterface::get_input()`
- Fixed draft clearing bug by emitting clear event before input clearing
- Implemented proper button state management with always-visible cancel button

### Week 2: Phase 1.2 - Chat Sidebar Events
- [ ] Create `ChatSidebarEvent` enum
- [ ] Update `ChatSidebar` to emit events instead of direct calls
- [ ] Update `RootView` to subscribe to chat events
- [ ] Test session selection and management

### Week 3: Phase 1.3 - Messages View Events
- [ ] Create `MessagesViewEvent` enum
- [ ] Update `MessagesView` to emit events
- [ ] Test message interactions

### Week 4: Phase 2.1 - Message Management
- [ ] Create `MessageManager` component with EventEmitter
- [ ] Replace `Arc<Mutex<Vec<Entity<MessageContainer>>>>` with EventEmitter
- [ ] Update all message-related components to subscribe
- [ ] Test message flow and streaming

### Week 5: Phase 2.2 - Memory View Integration
- [ ] Create `MemoryEvent` enum
- [ ] Update `MemoryView` to use EventEmitter
- [ ] Replace shared memory state with events
- [ ] Test memory view updates

### Week 6: Phase 3.1 - Event Hub
- [ ] Create central `EventHub` component
- [ ] Implement event routing logic
- [ ] Begin migration from global `UiEventSender`

### Week 7: Phase 3.2 - Typed Events Migration
- [ ] Create specific event type enums
- [ ] Implement event translation layer
- [ ] Migrate components one by one
- [ ] Remove old `UiEvent` enum

### Week 8: Testing and Cleanup
- [ ] Comprehensive testing of all event flows
- [ ] Performance testing and optimization
- [ ] Code cleanup and documentation
- [ ] Remove unused `Arc<Mutex<T>>` patterns

---

## Risk Mitigation

### Low Risk Changes
- Adding new EventEmitter implementations alongside existing code
- Creating wrapper components that delegate to existing components
- Adding new event types without removing old ones

### Medium Risk Changes
- Replacing direct method calls with event subscriptions
- Removing shared state `Arc<Mutex<T>>` patterns
- Modifying component initialization order

### High Risk Changes
- Removing global `UiEventSender`
- Changing the `UiEvent` enum structure
- Modifying UI ↔ Backend communication patterns

### Rollback Strategy
- Keep old communication patterns alongside new ones during transition
- Use feature flags to switch between old and new implementations
- Maintain comprehensive tests for both patterns during migration
- Only remove old code after new implementation is fully tested

---

## Expected Benefits

### Code Quality
- **Loose Coupling**: Components communicate via well-defined event interfaces
- **Testability**: Components can be easily mocked and unit tested
- **Maintainability**: Clear separation of concerns and event contracts

### Performance
- **Reduced Lock Contention**: Less `Arc<Mutex<T>>` usage reduces lock contention
- **Event Batching**: Multiple events can be processed in batches
- **Selective Updates**: Components only update when relevant events occur

### Developer Experience
- **Type Safety**: Compile-time guarantees for event handling
- **Discoverability**: Easy to find what events a component emits/handles
- **Debugging**: Clear event flow makes debugging easier

### Architecture
- **Scalability**: Easy to add new components and event types
- **Consistency**: Uniform communication patterns across all components
- **Separation of Concerns**: Clear boundaries between UI and business logic

---

## Progress Summary

### Completed: Phase 1.1 - InputArea Component Extraction

**Status**: ✓ Successfully completed and tested

**Achievements**:
- Created self-contained `InputArea` component with `EventEmitter<InputAreaEvent>`
- Removed tight coupling between `RootView` and input/attachment management
- Implemented robust event-driven communication with 5 distinct event types
- Fixed draft clearing race condition using synchronous `ClearDraftRequested` event
- Maintained V1 mode compatibility for legacy `get_input()` functionality
- Improved UI consistency with always-visible cancel button
- Reduced code complexity in `RootView` by ~200 lines
- Achieved clean separation of concerns with clear event boundaries

**Technical Impact**:
- Eliminated direct field access to input state from RootView
- Removed complex input/attachment handling logic from RootView
- Created reusable InputArea component for potential future use
- Established pattern for other component extractions

---

## Success Metrics

1. **Reduced Complexity**: ✓ Removed input/attachment fields and methods from RootView
2. **Improved Test Coverage**: ✓ InputArea can now be unit tested in isolation
3. **Better Performance**: ✓ Eliminated some direct state access patterns
4. **Code Maintainability**: ✓ Clear event boundaries make input functionality easier to modify
5. **Type Safety**: ✓ Compile-time event type guarantees for input communication

**Next Phase Ready**: The successful completion of InputArea extraction demonstrates the viability of the EventEmitter pattern and provides a template for extracting other components like ChatSidebar and MessagesView.

This refactoring has begun the transformation from a tightly-coupled, shared-state architecture to a loosely-coupled, event-driven architecture that is more maintainable, testable, and scalable.
