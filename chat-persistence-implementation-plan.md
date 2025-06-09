# Chat Persistence Implementation Plan

## Overview

This document outlines a comprehensive plan to implement persistent chat functionality in the code-assistant project. The feature will allow users to save, restore, and manage multiple chat sessions, with full restoration of message history, tool execution results, and working memory state.

## Current Architecture Analysis

### Message History and Tool Outputs
- **Message History**: Currently stored in `Agent.message_history: Vec<Message>`
- **Tool Executions**: Stored in `Agent.tool_executions: Vec<ToolExecution>`
- **Tool Results**: Each `ToolExecution` contains a `result: Box<dyn AnyOutput>` that supports:
  - Serialization via `to_json()` method
  - Deserialization via `DynTool::deserialize_output()`
  - Dynamic rendering via `as_render().render()`

### Current Persistence
- **State**: Saved in `AgentState` with task and messages only
- **Location**: Single file `.code-assistant.state.json` in project root
- **Scope**: Limited to current task continuation

### Working Memory Components
- File trees for each project
- Loaded resources (files, web search results, web pages)
- Resource summaries
- Available projects
- Current task and plan

## Implementation Plan

### Phase 1: Extended State Structure

#### 1.1 Enhanced AgentState
```rust
// crates/code_assistant/src/persistence.rs

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatSession {
    /// Unique identifier for the chat session
    pub id: String,
    /// User-friendly name for the chat
    pub name: String,
    /// Creation timestamp
    pub created_at: SystemTime,
    /// Last updated timestamp
    pub updated_at: SystemTime,
    /// Message history
    pub messages: Vec<Message>,
    /// Serialized tool execution results
    pub tool_executions: Vec<SerializedToolExecution>,
    /// Working memory state
    pub working_memory: WorkingMemory,
    /// Initial project path (if any)
    pub init_path: Option<PathBuf>,
    /// Initial project name
    pub initial_project: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SerializedToolExecution {
    /// Tool request details
    pub tool_request: ToolRequest,
    /// Serialized tool result as JSON
    pub result_json: serde_json::Value,
    /// Tool name for deserialization
    pub tool_name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatMetadata {
    pub id: String,
    pub name: String,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    pub message_count: usize,
}
```

#### 1.2 Enhanced StatePersistence Trait
```rust
pub trait StatePersistence: Send + Sync {
    // Chat session methods
    fn save_chat_session(&mut self, session: &ChatSession) -> Result<()>;
    fn load_chat_session(&mut self, session_id: &str) -> Result<Option<ChatSession>>;
    fn list_chat_sessions(&self) -> Result<Vec<ChatMetadata>>;
    fn delete_chat_session(&mut self, session_id: &str) -> Result<()>;
    fn get_latest_session_id(&self) -> Result<Option<String>>;
}
```

### Phase 2: Enhanced Persistence Implementation

#### 2.1 File-based Chat Storage
```rust
// crates/code_assistant/src/persistence.rs

pub struct FileStatePersistence {
    root_dir: PathBuf,
    chats_dir: PathBuf,
}

impl FileStatePersistence {
    pub fn new(root_dir: PathBuf) -> Self {
        let chats_dir = root_dir.join(".code-assistant-chats");
        Self { root_dir, chats_dir }
    }

    fn ensure_chats_dir(&self) -> Result<()> {
        if !self.chats_dir.exists() {
            std::fs::create_dir_all(&self.chats_dir)?;
        }
        Ok(())
    }

    fn chat_file_path(&self, session_id: &str) -> PathBuf {
        self.chats_dir.join(format!("{}.json", session_id))
    }

    fn metadata_file_path(&self) -> PathBuf {
        self.chats_dir.join("metadata.json")
    }
}
```

#### 2.2 Tool Execution Serialization Support
```rust
// crates/code_assistant/src/agent/types.rs

impl ToolExecution {
    pub fn serialize(&self) -> Result<SerializedToolExecution> {
        Ok(SerializedToolExecution {
            tool_request: self.tool_request.clone(),
            result_json: self.result.to_json()?,
            tool_name: self.tool_request.name.clone(),
        })
    }
}

impl SerializedToolExecution {
    pub fn deserialize(&self) -> Result<ToolExecution> {
        let tool = ToolRegistry::global()
            .get(&self.tool_name)
            .ok_or_else(|| anyhow::anyhow!("Tool not found: {}", self.tool_name))?;

        let result = tool.deserialize_output(self.result_json.clone())?;

        Ok(ToolExecution {
            tool_request: self.tool_request.clone(),
            result,
        })
    }
}
```

### Phase 3: Session Manager Architecture

#### 3.1 Dedicated Session Manager
```rust
// crates/code_assistant/src/session/mod.rs

pub struct SessionManager {
    persistence: Box<dyn StatePersistence>,
    current_session_id: Option<String>,
}

impl SessionManager {
    pub fn new(persistence: Box<dyn StatePersistence>) -> Self {
        Self {
            persistence,
            current_session_id: None,
        }
    }

    /// Create a new chat session and return its ID
    pub fn create_session(&mut self, name: Option<String>) -> Result<String> {
        let session_id = generate_session_id();
        let session_name = name.unwrap_or_else(|| format!("Chat {}", session_id[..8]));

        let session = ChatSession {
            id: session_id.clone(),
            name: session_name,
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
            messages: Vec::new(),
            tool_executions: Vec::new(),
            working_memory: WorkingMemory::default(),
            init_path: None,
            initial_project: None,
        };

        self.persistence.save_chat_session(&session)?;
        self.current_session_id = Some(session_id.clone());

        Ok(session_id)
    }

    /// Save current agent state to the active session
    pub fn save_session(&mut self,
        messages: Vec<Message>,
        tool_executions: Vec<ToolExecution>,
        working_memory: WorkingMemory,
        init_path: Option<PathBuf>,
        initial_project: Option<String>,
    ) -> Result<()> {
        let session_id = self.current_session_id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No active session"))?;

        let mut session = self.persistence
            .load_chat_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session not found"))?;

        // Update session with current state
        session.messages = messages;
        session.tool_executions = tool_executions
            .into_iter()
            .map(|te| te.serialize())
            .collect::<Result<Vec<_>>>()?;
        session.working_memory = working_memory;
        session.init_path = init_path;
        session.initial_project = initial_project;
        session.updated_at = SystemTime::now();

        self.persistence.save_chat_session(&session)?;
        Ok(())
    }

    /// Load a session and return its state for agent restoration
    pub fn load_session(&mut self, session_id: &str) -> Result<SessionState> {
        let session = self.persistence
            .load_chat_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

        self.current_session_id = Some(session_id.to_string());

        let tool_executions = session.tool_executions
            .into_iter()
            .map(|se| se.deserialize())
            .collect::<Result<Vec<_>>>()?;

        Ok(SessionState {
            messages: session.messages,
            tool_executions,
            working_memory: session.working_memory,
            init_path: session.init_path,
            initial_project: session.initial_project,
        })
    }

    /// List all available chat sessions
    pub fn list_sessions(&self) -> Result<Vec<ChatMetadata>> {
        self.persistence.list_chat_sessions()
    }
}

#[derive(Debug)]
pub struct SessionState {
    pub messages: Vec<Message>,
    pub tool_executions: Vec<ToolExecution>,
    pub working_memory: WorkingMemory,
    pub init_path: Option<PathBuf>,
    pub initial_project: Option<String>,
}
```

#### 3.2 Agent State Restoration Interface
```rust
// crates/code_assistant/src/agent/runner.rs

impl Agent {
    /// Load state from session manager (replaces start_from_state)
    pub async fn load_from_session_state(&mut self, session_state: SessionState) -> Result<()> {
        // Restore all state components
        self.message_history = session_state.messages;
        self.tool_executions = session_state.tool_executions;
        self.working_memory = session_state.working_memory;
        self.init_path = session_state.init_path;
        self.initial_project = session_state.initial_project;

        // Restore working memory file trees and project state
        self.restore_working_memory_state().await?;

        // Notify UI of restored state
        self.ui.display(UIMessage::Action(format!(
            "Loaded chat session with {} messages and {} tool executions",
            self.message_history.len(),
            self.tool_executions.len()
        ))).await?;

        let _ = self.ui.update_memory(&self.working_memory).await;

        Ok(())
    }

    /// Get current agent state for session saving
    pub fn get_current_state(&self) -> SessionState {
        SessionState {
            messages: self.message_history.clone(),
            tool_executions: self.tool_executions.clone(),
            working_memory: self.working_memory.clone(),
            init_path: self.init_path.clone(),
            initial_project: self.initial_project.clone(),
        }
    }

    /// Remove session-specific methods and state management from Agent
    /// The Agent should only be concerned with executing the current conversation
}
```

### Phase 4: Command Line Integration

#### 4.1 Enhanced Command Line Arguments
```rust
// crates/code_assistant/src/main.rs

#[derive(Parser, Debug)]
struct Args {
    // ... existing fields

    /// Resume a specific chat session by ID
    #[arg(long)]
    chat_id: Option<String>,

    /// List available chat sessions
    #[arg(long)]
    list_chats: bool,

    /// Create a new chat session (even if others exist)
    #[arg(long)]
    new_chat: bool,
}
```

#### 4.2 Chat Management Commands
```rust
async fn handle_chat_commands(args: &Args) -> Result<()> {
    let root_path = args.path.clone().unwrap_or_else(|| PathBuf::from("."));
    let persistence = Box::new(FileStatePersistence::new(root_path));
    let session_manager = SessionManager::new(persistence);

    if args.list_chats {
        let sessions = session_manager.list_sessions()?;
        if sessions.is_empty() {
            println!("No chat sessions found.");
        } else {
            println!("Available chat sessions:");
            for session in sessions {
                println!("  {} - {} ({} messages, created {})",
                    session.id,
                    session.name,
                    session.message_count,
                    format_time(session.created_at)
                );
            }
        }
        return Ok(());
    }

    // Handle other chat-related commands...
}
```

### Phase 5: UI Integration - Chat Sidebar

#### 5.1 UI-Agent Communication Architecture

The chat sidebar integration requires careful consideration of the communication patterns between the UI thread and the agent thread:

**Key Communication Challenges:**
- **Thread Safety**: The UI runs on the main thread while the agent runs on a separate thread
- **State Synchronization**: Chat list updates need to be reflected in the UI without blocking either thread
- **Event Handling**: User interactions in the chat sidebar must trigger agent operations asynchronously
- **Error Handling**: UI must handle cases where session loading fails or sessions become corrupted

**Communication Patterns:**

1. **UI → Agent Communication**:
   - Use existing event system (`UiEvent`) to send chat operation requests
   - Chat selection, new chat creation, and session deletion should be handled via events
   - Events should be queued and processed by the agent thread when appropriate

2. **Agent → UI Communication**:
   - Session list updates should be pushed to UI via the existing event system
   - Use shared state (Arc<Mutex<>>) for chat metadata that UI can read
   - Implement periodic refresh of chat list when sessions are created/modified

3. **Session Manager Integration**:
   - Session Manager should be owned by the main application, not the Agent
   - Agent should receive session state through dependency injection
   - UI should interact with Session Manager through the main application layer

**Implementation Considerations:**

- **Component Structure**: Chat sidebar should be a separate GPUI component similar to existing MemoryView
- **Event Flow**: UI events should flow through the main application to the Session Manager, then to the Agent
- **State Management**: Use the existing pattern of Arc<Mutex<Option<T>>> for shared state between threads
- **Layout Integration**: Left sidebar for chats, center for messages, right sidebar for memory (existing)
- **Responsive Design**: Sidebar should be collapsible similar to the existing memory sidebar

**Error Handling Patterns:**
- **Session Load Failures**: Should display error in UI without crashing the application
- **Corrupted Sessions**: Should be handled gracefully with user notification
- **Thread Communication Failures**: Should have fallback mechanisms for UI responsiveness

#### 5.2 UI Event System Extensions

The existing `UiEvent` system needs to be extended to handle chat operations:

**New Event Types Needed:**
- `LoadChatSession { session_id: String }`
- `CreateNewChatSession { name: Option<String> }`
- `DeleteChatSession { session_id: String }`
- `RefreshChatList`
- `UpdateChatList { sessions: Vec<ChatMetadata> }`

**Event Processing Considerations:**
- Chat operations should be processed before agent loop operations
- Long-running session operations should not block the agent loop
- UI should show loading states during session transitions

### Phase 6: GPUI Integration

#### 6.1 Main Application Integration

The main application layer needs to coordinate between the Session Manager, Agent, and UI:

**Application Architecture Considerations:**

- **Ownership**: Session Manager should be owned at the application level, not within Agent or UI
- **Coordination**: Main application handles session operations and delegates to appropriate components
- **State Flow**: Session state flows from Session Manager → Application → Agent
- **UI Updates**: Chat list updates flow from Session Manager → Application → UI

**Integration Points:**

1. **Application Startup**: Initialize Session Manager alongside other core components
2. **Event Processing**: Main event loop should handle chat-related events before delegating to agent
3. **Session Transitions**: Coordinate between stopping current agent operations and loading new session
4. **Error Propagation**: Ensure session-related errors are properly communicated to UI

#### 6.2 Enhanced UI Event System

The UI event system needs to be extended to support chat operations while maintaining the existing architecture:

**Event Processing Flow:**
1. UI generates chat events (load session, new chat, etc.)
2. Events are queued in the existing event system
3. Main application processes chat events and coordinates with Session Manager
4. Agent receives session state and continues with normal operation
5. UI receives updates about session changes and updates chat list

**Threading Considerations:**
- Session loading should happen on a background thread to avoid blocking UI
- UI should show loading indicators during session transitions
- Agent thread should be paused/restarted when switching sessions

### Phase 7: Session ID Generation and Utilities

#### 7.1 Utility Functions
```rust
// crates/code_assistant/src/persistence.rs

use uuid::Uuid;
use chrono::{DateTime, Utc};

pub fn generate_session_id() -> String {
    format!("chat_{}", Uuid::new_v4().to_string().replace('-', "")[..12].to_lowercase())
}

pub fn extract_name_from_first_message(messages: &[Message]) -> String {
    if let Some(first_message) = messages.first() {
        match &first_message.content {
            MessageContent::Text(text) => {
                let max_len = 50;
                if text.len() <= max_len {
                    text.clone()
                } else {
                    format!("{}...", &text[..max_len-3])
                }
            }
            MessageContent::Structured(_) => "New Chat".to_string(),
        }
    } else {
        "Empty Chat".to_string()
    }
}

pub fn format_time(time: SystemTime) -> String {
    let datetime: DateTime<Utc> = time.into();
    datetime.format("%Y-%m-%d %H:%M").to_string()
}
```

### Phase 8: Advanced Features (Future Enhancements)

## Implementation Phases Summary

### Phase 1: Core Infrastructure (Week 1)
- [ ] `ChatSession` and `SerializedToolExecution` structures
- [ ] Enhanced `StatePersistence` trait with chat session methods
- [ ] Tool execution serialization/deserialization support

### Phase 2: Persistence Layer (Week 1-2)
- [ ] File-based chat storage implementation
- [ ] Metadata management system

### Phase 3: Session Manager Architecture (Week 2)
- [ ] Dedicated `SessionManager` component separate from Agent
- [ ] Session CRUD operations (create, load, save, delete)
- [ ] Agent state restoration interface

### Phase 4: Command Line Interface (Week 2-3)
- [ ] Extended command line arguments for chat management
- [ ] Chat listing and selection commands
- [ ] New chat session creation

### Phase 5: UI Architecture and Communication (Week 3-4)
- [ ] UI-Agent communication patterns for chat operations
- [ ] Chat sidebar component design and integration
- [ ] Event system extensions for chat operations

### Phase 6: Application Integration (Week 4)
- [ ] Main application coordination between Session Manager, Agent, and UI
- [ ] Session transition handling
- [ ] Error handling and recovery

### Phase 7: Polish and Testing (Week 4-5)
- [ ] Comprehensive testing of all chat operations
- [ ] Error handling and edge cases
- [ ] Documentation and user guides

### Phase 8: Advanced Features (Future)
- [ ] Chat search and filtering
- [ ] Export/import chat sessions
- [ ] Chat session sharing
- [ ] Chat session templates

## Technical Considerations

### Performance
- **Working Memory**: Optimize working memory restoration to avoid redundant file system operations
- **UI Responsiveness**: Use background tasks for chat loading and saving operations

### Error Handling
- **Corrupted Sessions**: Graceful handling of corrupted chat session files
- **Tool Compatibility**: Handle cases where tools have changed between sessions
- **UI State**: Robust error recovery in UI components

### Security
- **File Permissions**: Ensure chat files are properly secured
- **Path Traversal**: Validate session IDs to prevent directory traversal attacks
- **Sensitive Data**: Consider encryption for sensitive chat content

### Extensibility
- **Sub-Agent Support**: Session Manager and Agent architecture designed to support future sub-agent functionality
- **Plugin Support**: Design chat session format to support future plugin data
- **Custom Metadata**: Allow tools to store custom metadata in sessions
- **Export Formats**: Support multiple export formats for chat sessions

This implementation plan provides a comprehensive roadmap for adding persistent chat functionality to the code-assistant project. The architecture separates concerns between session management and agent execution, enabling future features like sub-agents while providing a rich user experience across both command-line and GUI interfaces.
