# Chat Persistence Implementation Plan - Updated

## Overview

This document outlines the implementation of persistent chat functionality in the code-assistant project. The feature allows users to save, restore, and manage multiple chat sessions, with full restoration of message history, tool execution results, and working memory state.

## ✅ Completed Implementation Status

### **✅ Phase 1: Extended State Structure (COMPLETED)**
- **ChatSession** structure with metadata, messages, tool executions, and working memory
- **SerializedToolExecution** for storing tool results
- **ChatMetadata** for session listing
- **Custom serialization** for HashMap with tuple keys `(String, PathBuf)` in WorkingMemory
- **Utility functions** for session ID generation and formatting

### **✅ Phase 3: Session Manager Architecture (COMPLETED)**
- **SessionManager** class independent from Agent
- **SessionState** for agent restoration
- **Complete CRUD operations** for chat sessions
- **Auto-session creation** when none exists
- **ToolExecution Clone implementation** using serialize/deserialize

### **✅ Phase 4: Command Line Integration (COMPLETED)**
- **CLI arguments**: `--chat-id`, `--list-chats`, `--delete-chat`, `--continue`
- **Removed `--new-chat`**: Every new task automatically creates new session
- **Smart session logic**: Load specific session, continue latest, or create new
- **Robust error handling** and validation

### **✅ Major Refactoring (COMPLETED)**
- **Completely removed old StatePersistence** trait and implementations
- **Agent directly uses SessionManager** instead of StatePersistence
- **All conversations automatically persisted** as chat sessions
- **Tests updated** to use SessionManager with unique temporary directories
- **No more `.code-assistant.state.json`** files

## 🎯 Current Architecture

```rust
// New simplified architecture:
SessionManager {
    persistence: FileStatePersistence,  // Direct, no trait
    current_session_id: Option<String>,
}

Agent {
    session_manager: SessionManager,    // No more StatePersistence
    // ... other fields
}

// Working usage:
./code-assistant --task "Review code"     // → Auto-creates new session
./code-assistant --chat-id chat_abc123    // → Loads specific session
./code-assistant --continue               // → Continues latest session
./code-assistant --list-chats             // → Lists all sessions
```

## 🔧 Key Technical Solutions

### **1. HashMap Serialization Issue**
**Problem**: `HashMap<(String, PathBuf), LoadedResource>` can't serialize to JSON (tuple keys not allowed)

**Solution**: Custom serde implementation that converts tuple keys to strings:
```rust
#[serde(with = "tuple_key_map")]
pub loaded_resources: HashMap<(String, PathBuf), LoadedResource>,

// Converts (project, path) ↔ "project::path"
```

### **2. ToolExecution Clone Issue**
**Problem**: `Box<dyn AnyOutput>` doesn't implement Clone automatically

**Solution**: Manual Clone implementation using serialize/deserialize:
```rust
impl Clone for ToolExecution {
    fn clone(&self) -> Self {
        let serialized = self.serialize().expect("Failed to serialize for cloning");
        serialized.deserialize().expect("Failed to deserialize for cloning")
    }
}
```

### **3. Tool Result Serialization Robustness**
**Problem**: Some tool results might not serialize properly

**Solution**: Fallback mechanism in serialize():
```rust
let result_json = match self.result.to_json() {
    Ok(json) => json,
    Err(e) => serde_json::json!({
        "error": "Failed to serialize result",
        "success": self.result.is_success(),
        "details": format!("{}", e)
    })
};
```

### **4. Test Race Conditions**
**Problem**: Tests using same temp directories causing conflicts

**Solution**: Unique timestamp-based directory names:
```rust
let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
let temp_dir = std::env::temp_dir().join(format!("code_assistant_test_{}_{}", std::process::id(), timestamp));
```

## 🚀 Current Implementation Status

### **✅ Phase 5: UI Integration - Chat Sidebar (COMPLETED)**

#### 5.1 GPUI Chat Components ✅
**Implemented in**: `crates/code_assistant/src/ui/gpui/chat_sidebar.rs`
```rust
pub struct ChatSidebar {
    sessions: Vec<ChatMetadata>,
    selected_session_id: Option<String>,
    is_collapsed: bool,
}

pub struct ChatListItem {
    metadata: ChatMetadata,
    is_selected: bool,
}
```

#### 5.2 UI Event System Extensions ✅
**Implemented in**: `crates/code_assistant/src/ui/gpui/ui_events.rs`
```rust
pub enum UiEvent {
    LoadChatSession { session_id: String },
    CreateNewChatSession { name: Option<String> },
    DeleteChatSession { session_id: String },
    RefreshChatList,
    UpdateChatList { sessions: Vec<ChatMetadata> },
    // ... existing events
}
```

#### 5.3 Layout Integration ✅
**Implemented in**: `crates/code_assistant/src/ui/gpui/root.rs`
- **Left sidebar**: Chat sessions list (260px, collapsible) ✅
- **Center**: Messages and input area (flexible width) ✅
- **Right sidebar**: Working memory (260px, collapsible) ✅
- **Window size**: Expanded to 1400x700px for 3-column layout ✅

#### 5.4 Bidirectional Communication ✅
**Implemented in**: `crates/code_assistant/src/ui/gpui/mod.rs`
```rust
// Chat management events between UI and Agent threads
pub enum ChatManagementEvent {
    LoadSession { session_id: String },
    CreateNewSession { name: Option<String> },
    DeleteSession { session_id: String },
    ListSessions,
}

pub enum ChatManagementResponse {
    SessionLoaded { session_id: String },
    SessionCreated { session_id: String, name: String },
    SessionDeleted { session_id: String },
    SessionsListed { sessions: Vec<ChatMetadata> },
    Error { message: String },
}
```

### **🔄 Phase 6: Application Integration (PARTIALLY COMPLETED)**

#### 6.1 Main Application Coordination ✅
**Implemented in**: `crates/code_assistant/src/main.rs` (lines ~350-430)
- Chat communication channels setup ✅
- Separate task for chat management events ✅
- Integration with existing Agent thread ✅

#### 6.2 Thread Communication ✅
**Implemented in**: `crates/code_assistant/src/ui/gpui/mod.rs`
- **UI Thread**: Handles UI events and chat responses ✅
- **Agent Thread**: Processes chat management events ✅
- **Communication**: async_channel for bidirectional messaging ✅

#### 6.3 UI Controls ✅
**Implemented in**: `crates/code_assistant/src/ui/gpui/root.rs`
- Chat sidebar toggle button in titlebar (💬 icon) ✅
- Automatic chat list loading on startup ✅
- Real-time UI updates for chat operations ✅

## ✅ Current Issues: RESOLVED!

### **🎉 Fixed Issues**
1. **✅ "+" Button Functionality**: Now working - click events properly handled
   - **Root Cause**: Used `sender.0.send()` instead of `sender.0.try_send()` in UI callbacks
   - **Fix**: Changed to `try_send()` for synchronous UI contexts

2. **✅ Chat List Display**: Now shows all 7 sessions correctly
   - **Root Cause**: Task handling problems - UI event processing task not staying alive
   - **Fix**: Changed `Arc<Mutex<Option<Box<dyn Any>>>>` to `Arc<Mutex<Option<gpui::Task<()>>>>`

3. **✅ Communication Pipeline**: Full event flow working
   - RefreshChatList: UI → Agent → Response → UI ✅
   - Session clicks: UI event processing working ✅
   - Plus button: UI event recognition working ✅

### **🔧 Technical Fixes Applied**
- **Task Management**: Proper `gpui::Task<()>` storage instead of type erasure
- **Agent Thread**: Chat management task handle stored with `_chat_management_task`
- **Event Sending**: `try_send()` for synchronous UI contexts, `send().await` for async
- **Event Processing**: Full pipeline working with comprehensive logging

## 🎯 Current Implementation Status: Session Loading Communication Fixed

### **✅ Architecture Problem Solved: Agent Blocking During Input Wait**

**Previous Problem**: Agent's `run_agent_loop()` was blocked when waiting for user input via `get_input()`, so `SwitchToSession` commands never reached the agent.

**Solution Implemented**:
- **Removed agent command processing from agent loop** - no more `try_recv()` calls that never execute
- **Session loading moved to Chat Management Task** - bypasses agent completely during loading
- **Simplified communication flow**: UI → Chat Management → SessionManager → UI

**New Flow:**
```
UI Click → LoadChatSession → Chat Management Task → load_session() → SessionLoaded Response → UI
```

### **✅ What Works Now**
- ✅ Chat sidebar displays all sessions
- ✅ Plus button sends CreateNewChatSession events
- ✅ Session clicks send LoadChatSession events
- ✅ **Communication works even when agent waits for input**
- ✅ Session loading succeeds (logs show "Loaded session X with N messages")
- ✅ Old messages clear from UI
- ✅ Session becomes active in sidebar

### **🚨 Critical Problem Identified: Agent State Desynchronization**

**Current Issue**: Session loading only updates the UI, but **Agent's message_history remains unchanged**.

**Result**:
- ✅ UI shows new session messages (when implemented)
- ❌ Agent still has old session's message_history
- ❌ New user messages get appended to wrong conversation
- ❌ Agent context is completely wrong

**Required Fix**: Agent state must be synchronized when session changes.

### **✅ Session Loading Implementation Status (Partially Complete)**

**What's Implemented:**
1. ✅ **UI Communication Fixed** - LoadChatSession works even when agent waits for input
2. ✅ **Session Loading in Chat Management Task** - Bypasses agent completely during loading
3. ✅ **SetMessages Event Generation** - `create_fragments_from_messages()` implemented in UI
4. ✅ **Fragment Creation Working** - Reuses StreamProcessor logic via `extract_fragments_from_message()`
5. ✅ **UI Message Display** - Loaded session messages now appear in UI correctly

**Current Files Modified:**
- `main.rs` (lines 421+): Chat Management Task loads sessions directly
- `ui/gpui/mod.rs` (lines 630+): `handle_chat_response()` processes loaded messages
- `ui/gpui/mod.rs` (lines 696+): `create_fragments_from_messages()` converts messages to fragments
- `ui/gpui/mod.rs` (lines 68): `ChatManagementResponse::SessionLoaded` includes messages

**New Architecture Flow:**
```
UI Click → LoadChatSession → Chat Management Task → SessionManager.load_session()
→ SessionLoaded{messages} → UI.create_fragments_from_messages() → SetMessages → UI Display
```

### **🚨 Critical Problem: Agent State Desynchronization**

**Issue**: UI shows correct messages from loaded session, but **Agent still has old session state**.

**Specific Problems:**
- ❌ Agent's `message_history` still contains old conversation
- ❌ Agent's `tool_executions` from wrong session
- ❌ Agent's `working_memory` from wrong session
- ❌ Next user input gets appended to wrong conversation
- ❌ Agent has completely wrong context for responses

**Result**: UI and Agent are completely out of sync after session switch.

### **🔮 Revolutionary Idea: Session-Based Agent Management**

**Current Architecture Problem:**
- Single Agent instance with mutable state
- Complex state synchronization required
- Agent loop blocks on input wait
- Session switching requires state juggling

**New Proposed Architecture:**
```rust
struct SessionManager {
    sessions: HashMap<String, SessionInstance>,
    active_session_id: Option<String>,
}

struct SessionInstance {
    agent: Option<Agent>,
    task_handle: Option<JoinHandle<()>>,
    state: SessionState,
    is_streaming: bool,
}
```

**Paradigm Shift: "Session Switching" → "Session Activation"**

**New Flow:**
1. **User Input** → SessionManager identifies target session
2. **Append Message** to session.state.messages
3. **Spawn Agent Loop** for this specific session (if not already running)
4. **Agent runs** until completion/error/stop → **terminates cleanly**
5. **No input wait blocking** - agent loops just end
6. **Session Switch** = activate different SessionInstance

**Revolutionary Benefits:**
- ✅ **Perfect State Isolation** - each session has its own Agent with own message_history
- ✅ **No State Synchronization** - no need to update agent state on session switch
- ✅ **No Blocking Issues** - agent loops terminate instead of waiting for input
- ✅ **True Parallel Streaming** - multiple sessions can stream simultaneously
- ✅ **Clean Session Switching** - just change active_session_id
- ✅ **Automatic Persistence** - session state managed by SessionInstance

**Advanced Features Possible:**
- Multiple sessions streaming in background
- UI switches between active streams
- Background sessions continue processing
- Per-session LLM provider/model settings
- Session pause/resume functionality

**Implementation Strategy:**
1. **SessionManager Enhancement** - manage Agent lifecycle per session
2. **Agent Lifecycle** - spawn/terminate pattern instead of persistent loop
3. **UI Events Enhanced** - include session_id in all streaming events
4. **Session Activation** - UI activates session rather than loading state

**Stream Switching Solution:**
```rust
// UI receives events with session context
UiEvent::StreamFragment { session_id, fragment }

// UI only processes events from active session
if session_id == self.active_session_id {
    self.display_fragment(fragment);
}
```

This architecture eliminates the entire class of state synchronization problems and enables much more powerful session management.

### **🎯 Next Session Roadmap**

**Option A: Fix Current Architecture**
- Implement Agent state synchronization
- Handle the complexity of state updates
- Still have blocking and single-session limitations

**Option B: Implement Session-Based Agent Management** ⭐ **RECOMMENDED**
- Revolutionary architecture upgrade
- Solves all current problems elegantly
- Enables advanced features like parallel streaming
- Much cleaner and more maintainable code

The Session-Based approach is significantly better long-term and solves the root architectural issues rather than patching symptoms.

**Current Message Flow:**
```
Streaming: LLM Chunks → StreamProcessor → DisplayFragments → UI Events → MessageContainer
```

**Needed for Session Loading:**
```
Session Load: Stored Messages → StreamProcessor.extract_fragments → DisplayFragments → UI Events → MessageContainer
```

### **🎯 Recommended Approach: StreamProcessor Trait Extension**

The key insight is that **stored messages need the same processing pipeline** as streaming messages. Both JSON and XML modes have complex parsing logic that we want to reuse.

**Current Problem:**
- **JSON Mode**: `ContentBlock::ToolUse` needs parsing into `DisplayFragment::ToolName + ToolParameter`
- **XML Mode**: `ContentBlock::Text` with `<tool:name><param:value>` needs XML parsing into DisplayFragments

**Solution: Extend StreamProcessorTrait**
```rust
pub trait StreamProcessorTrait: Send + Sync {
    fn new(ui: Arc<Box<dyn UserInterface>>) -> Self where Self: Sized;
    fn process(&mut self, chunk: &StreamingChunk) -> Result<(), UIError>;

    // NEW: Extract fragments without sending to UI
    fn extract_fragments_from_message(&mut self, message: &Message) -> Result<Vec<DisplayFragment>, UIError>;
}
```

### **Implementation Strategy**

#### **Phase 1: StreamProcessor Enhancement**
**Files to modify:**
- `crates/code_assistant/src/ui/streaming/mod.rs` - Add trait method
- `crates/code_assistant/src/ui/streaming/json_processor.rs` - Implement for JSON mode
- `crates/code_assistant/src/ui/streaming/xml_processor.rs` - Implement for XML mode

**JSON Implementation:**
```rust
// JsonStreamProcessor::extract_fragments_from_message()
match &message.content {
    MessageContent::Structured(blocks) => {
        for block in blocks {
            ContentBlock::ToolUse { id, name, input } => {
                fragments.push(DisplayFragment::ToolName { name, id });
                // Parse input JSON to ToolParameter fragments
            }
            ContentBlock::Text { text } => fragments.push(DisplayFragment::PlainText(text)),
            ContentBlock::Thinking { thinking, .. } => fragments.push(DisplayFragment::ThinkingText(thinking)),
        }
    }
}
```

**XML Implementation:**
```rust
// XmlStreamProcessor::extract_fragments_from_message()
// Use existing process_text_with_tags() logic but collect fragments instead of sending to UI
```

#### **Phase 2: New UI Events**
**Files to modify:**
- `crates/code_assistant/src/ui/gpui/ui_events.rs` - Add new events
- `crates/code_assistant/src/ui/gpui/mod.rs` - Handle new events

**New Events:**
```rust
pub enum UiEvent {
    // Existing events...

    // Clear all messages and load new ones
    LoadSessionFragments { fragments: Vec<DisplayFragment> },

    // Clear messages only
    ClearMessages,
}
```

#### **Phase 3: SessionManager Integration**
**Files to modify:**
- `crates/code_assistant/src/session/mod.rs` - Add load_session_fragments method
- `crates/code_assistant/src/main.rs` - Handle LoadChatSession event properly

**Session Loading Flow:**
```rust
// In chat management task (main.rs)
ChatManagementEvent::LoadSession { session_id } => {
    let session = session_manager.load_session(&session_id)?;
    let tool_mode = agent.get_tool_mode(); // Need access to current tool mode
    let mut processor = create_stream_processor(tool_mode, dummy_ui);

    let mut all_fragments = Vec::new();
    for message in session.messages {
        let fragments = processor.extract_fragments_from_message(&message)?;
        all_fragments.extend(fragments);
    }

    ChatManagementResponse::SessionFragmentsReady { session_id, fragments: all_fragments }
}
```

#### **Phase 4: UI MessageContainer Management**
**Files to modify:**
- `crates/code_assistant/src/ui/gpui/mod.rs` - process LoadSessionFragments event
- `crates/code_assistant/src/ui/gpui/messages.rs` - Add clear_messages method

**Entity Lifecycle:**
```rust
// In process_ui_event_async()
UiEvent::LoadSessionFragments { fragments } => {
    // Clear existing message queue
    self.message_queue.lock().unwrap().clear();

    // Process fragments as if they were streaming
    for fragment in fragments {
        self.process_display_fragment(fragment);
    }
}
```

### **Key Benefits of This Approach**

1. **Zero Code Duplication**: Reuses existing JSON/XML parsing logic
2. **Consistent Behavior**: Session loading looks identical to live streaming
3. **Tool Mode Agnostic**: Works for both JSON and XML automatically
4. **Clean Architecture**: Separates fragment extraction from UI sending
5. **Easy Testing**: Can test fragment extraction independently

### **Technical Challenges & Solutions**

1. **Tool Mode Detection**: Need to know which StreamProcessor to use
   - **Solution**: Store tool_mode in ChatSession metadata or get from Agent

2. **Fragment Ordering**: Ensure fragments are processed in correct order
   - **Solution**: Messages are ordered, fragments within message are ordered

3. **Entity Management**: Proper MessageContainer lifecycle
   - **Solution**: Clear existing entities, rebuild from fragments

4. **State Synchronization**: Working memory, current session, etc.
   - **Solution**: Separate events for different state aspects

### **Implementation Priority**
1. **Complete CreateNewChatSession flow** (minor agent-side fix needed)
2. **Extend StreamProcessorTrait** (add extract_fragments_from_message method)
3. **Implement JSON/XML fragment extraction** (reuse existing parsing logic)
4. **Add LoadSessionFragments UI event** (clear + rebuild message containers)
5. **Integrate with SessionManager** (modify chat management task)
6. **Test with real sessions** (both JSON and XML mode sessions)

### **Files Overview for Implementation**

**Core Stream Processing:**
- `crates/code_assistant/src/ui/streaming/mod.rs` - Trait definition
- `crates/code_assistant/src/ui/streaming/json_processor.rs` - JSON implementation
- `crates/code_assistant/src/ui/streaming/xml_processor.rs` - XML implementation

**UI Events & Processing:**
- `crates/code_assistant/src/ui/gpui/ui_events.rs` - Event definitions
- `crates/code_assistant/src/ui/gpui/mod.rs` - Event processing logic
- `crates/code_assistant/src/ui/gpui/messages.rs` - MessageContainer management

**Session Management:**
- `crates/code_assistant/src/session/mod.rs` - Session loading logic
- `crates/code_assistant/src/main.rs` - Chat management task (agent thread)
- `crates/code_assistant/src/persistence.rs` - ChatSession structure (if needed)

### **Enhancement Tasks (Future Sessions)**

#### Storage Architecture Improvements
1. **Global Session Storage**
   - **Current**: Sessions stored in current working directory
   - **Target**: Global storage location (e.g., `~/.code-assistant/sessions/`)
   - **Files to modify**:
     - `crates/code_assistant/src/persistence.rs`
     - `crates/code_assistant/src/main.rs`

2. **Enhanced Session Metadata**
   - **Current project path** persistence in session
   - **LLM provider and model** storage per session
   - **Session-specific configurations**
   - **Files to modify**:
     - `crates/code_assistant/src/persistence.rs` (ChatSession struct)
     - `crates/code_assistant/src/session/mod.rs`

#### Advanced Features
3. **Session Management Features**
   - Delete session functionality
   - Session renaming
   - Session duplication
   - Session export/import

4. **UI Enhancements**
   - Session context menu (right-click)
   - Drag & drop session reordering
   - Session search/filter
   - Recent sessions quick access

## 🧪 Testing Strategy

### **Unit Tests (Already Fixed)**
- ✅ Agent tests with SessionManager
- ✅ Unique temporary directories
- ✅ Robust serialization testing

### **Integration Tests (Needed)**
- [ ] End-to-end session creation and restoration
- [ ] UI event handling for chat operations
- [ ] Multi-session workflow testing
- [ ] Error handling and recovery

### **Manual Testing Scenarios**
- [ ] Create multiple chat sessions with different tasks
- [ ] Switch between sessions and verify state restoration
- [ ] Delete sessions and verify cleanup
- [ ] Test with large sessions (many messages/tools)

## 📋 Implementation Checklist

### **✅ Phase 5: UI Integration (COMPLETED)**
- [x] Create ChatSidebar component (`crates/code_assistant/src/ui/gpui/chat_sidebar.rs`)
- [x] Implement ChatListItem component (inline in ChatSidebar)
- [x] Add chat-related UiEvents (`crates/code_assistant/src/ui/gpui/ui_events.rs`)
- [x] Integrate with existing GPUI layout (`crates/code_assistant/src/ui/gpui/root.rs`)
- [x] Add session switching functionality (events implemented)
- [x] Implement real-time session list updates (communication channels)

### **🔄 Phase 6: Application Integration (DEBUGGING NEEDED)**
- [x] Update main application to coordinate SessionManager (`crates/code_assistant/src/main.rs`)
- [x] Implement thread-safe session communication (async channels)
- [x] Add proper error handling for UI operations (ChatManagementResponse::Error)
- [🐛] **DEBUG NEEDED**: Event propagation and UI state sync
- [ ] Add session transition animations/feedback
- [ ] Comprehensive testing

### **🎯 Enhancement Tasks (FUTURE)**
- [ ] Global session storage architecture
- [ ] Enhanced session metadata (project path, LLM provider/model)
- [ ] Session delete functionality
- [ ] Session search and filtering
- [ ] Add keyboard shortcuts for session management
- [ ] Performance optimization for large session lists
- [ ] Documentation updates

## 🎯 Success Criteria

1. **✅ Automatic Persistence**: Every conversation automatically saved as chat session
2. **✅ CLI Management**: Full session management via command line
3. **✅ State Restoration**: Complete restoration of messages, tools, and working memory
4. **🔄 UI Integration**: Chat sidebar visible but needs debugging for functionality
5. **❌ Seamless Switching**: Not yet functional - requires debugging
6. **🔄 Error Recovery**: Basic error handling implemented, needs testing

## 📝 Notes and Lessons Learned

### **Architecture Decisions**
- **Simplified over abstracted**: Removed StatePersistence trait for direct FileStatePersistence
- **Auto-creation over explicit**: Every new task creates session automatically
- **Direct integration over delegation**: Agent owns SessionManager directly

### **Technical Gotchas**
- **JSON Serialization**: HashMap with non-string keys needs custom serde
- **Test Isolation**: Unique temporary directories essential for concurrent tests
- **Tool Result Storage**: Need fallback for non-serializable tool outputs
- **Clone Semantics**: Manual implementation needed for trait objects

### **UX Simplifications**
- **No --new-chat flag**: Simpler mental model without explicit new chat creation
- **Smart defaults**: Continue latest session when no specific session specified
- **Consistent CLI**: All chat operations follow same pattern

## 📁 Key File Locations

### **Core Chat Persistence**
- `crates/code_assistant/src/session/mod.rs` - SessionManager implementation
- `crates/code_assistant/src/persistence.rs` - ChatSession, ChatMetadata structs
- `crates/code_assistant/src/agent/runner.rs` - Agent integration with sessions

### **UI Components**
- `crates/code_assistant/src/ui/gpui/chat_sidebar.rs` - Chat sidebar component
- `crates/code_assistant/src/ui/gpui/root.rs` - Main layout with 3-column design
- `crates/code_assistant/src/ui/gpui/ui_events.rs` - UI event definitions
- `crates/code_assistant/src/ui/gpui/mod.rs` - Main GPUI implementation & communication

### **Integration**
- `crates/code_assistant/src/main.rs` - Application entry point & thread setup
- `crates/code_assistant/src/ui/gpui/file_icons.rs` - Icon constants (MESSAGE_BUBBLES, PLUS)

### **Assets**
- `crates/code_assistant/assets/icons/message_bubbles.svg` - Chat sidebar icon
- `crates/code_assistant/assets/icons/plus.svg` - New chat button icon

This updated plan reflects the current implementation state and provides clear next steps for completing the chat persistence feature.
