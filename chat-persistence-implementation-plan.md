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
    /// Original task description
    pub task: String,
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
    pub task: String,
    pub message_count: usize,
}
```

#### 1.2 Enhanced StatePersistence Trait
```rust
pub trait StatePersistence: Send + Sync {
    // Legacy methods (keep for backward compatibility)
    fn save_state(&mut self, task: String, messages: Vec<Message>) -> Result<()>;
    fn load_state(&mut self) -> Result<Option<AgentState>>;
    fn cleanup(&mut self) -> Result<()>;
    
    // New chat session methods
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

### Phase 3: Agent Integration

#### 3.1 Enhanced Agent Constructor
```rust
// crates/code_assistant/src/agent/runner.rs

impl Agent {
    pub fn new_with_session_id(
        // ... existing parameters
        session_id: Option<String>,
    ) -> Self {
        Self {
            // ... existing fields
            current_session_id: session_id,
            session_manager: SessionManager::new(),
        }
    }
}
```

#### 3.2 Session Management
```rust
pub struct SessionManager {
    current_session: Option<ChatSession>,
}

impl Agent {
    /// Save current state as a chat session
    pub fn save_current_session(&mut self, name: Option<String>) -> Result<String> {
        let session_id = self.current_session_id
            .clone()
            .unwrap_or_else(|| generate_session_id());
            
        let session_name = name.unwrap_or_else(|| {
            truncate_task_for_name(&self.working_memory.current_task)
        });
        
        let session = ChatSession {
            id: session_id.clone(),
            name: session_name,
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
            task: self.working_memory.current_task.clone(),
            messages: self.message_history.clone(),
            tool_executions: self.serialize_tool_executions()?,
            working_memory: self.working_memory.clone(),
            init_path: self.init_path.clone(),
            initial_project: self.initial_project.clone(),
        };
        
        self.state_persistence.save_chat_session(&session)?;
        self.current_session_id = Some(session_id.clone());
        
        Ok(session_id)
    }
    
    /// Load a chat session by ID
    pub async fn load_session(&mut self, session_id: &str) -> Result<()> {
        let session = self.state_persistence
            .load_chat_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;
            
        self.restore_from_session(session).await
    }
    
    /// Start a new chat session
    pub async fn start_new_session(&mut self, task: String) -> Result<()> {
        // Clear current state
        self.reset_state();
        
        // Generate new session ID
        self.current_session_id = Some(generate_session_id());
        
        // Start with new task
        self.start_with_task(task).await
    }
    
    async fn restore_from_session(&mut self, session: ChatSession) -> Result<()> {
        // Restore basic state
        self.current_session_id = Some(session.id.clone());
        self.working_memory = session.working_memory;
        self.message_history = session.messages;
        self.init_path = session.init_path;
        self.initial_project = session.initial_project;
        
        // Restore tool executions
        self.tool_executions = session.tool_executions
            .into_iter()
            .map(|se| se.deserialize())
            .collect::<Result<Vec<_>>>()?;
        
        // Restore working memory state
        self.restore_working_memory_state().await?;
        
        // Notify UI
        self.ui.display(UIMessage::Action(format!(
            "Restored chat session: {} ({} messages, {} tool executions)",
            session.name,
            self.message_history.len(),
            self.tool_executions.len()
        ))).await?;
        
        // Update UI with restored state
        let _ = self.ui.update_memory(&self.working_memory).await;
        
        Ok(())
    }
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
    let mut persistence = FileStatePersistence::new(root_path);
    
    if args.list_chats {
        let sessions = persistence.list_chat_sessions()?;
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

#### 5.1 Chat List Component
```rust
// crates/code_assistant/src/ui/gpui/chat_sidebar.rs

pub struct ChatSidebar {
    pub sessions: Vec<ChatMetadata>,
    pub selected_session: Option<String>,
    pub events: Arc<Mutex<async_channel::Sender<ChatSidebarEvent>>>,
    focus_handle: FocusHandle,
}

#[derive(Debug, Clone)]
pub enum ChatSidebarEvent {
    LoadSession(String),
    DeleteSession(String),
    NewChat,
    RenameSession(String, String),
}

impl ChatSidebar {
    pub fn new(
        events: Arc<Mutex<async_channel::Sender<ChatSidebarEvent>>>,
        cx: &mut Context<Self>
    ) -> Self {
        Self {
            sessions: Vec::new(),
            selected_session: None,
            events,
            focus_handle: cx.focus_handle(),
        }
    }
    
    pub fn update_sessions(&mut self, sessions: Vec<ChatMetadata>) {
        self.sessions = sessions;
    }
    
    fn on_session_click(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.selected_session = Some(session_id.clone());
        let _ = self.events.lock().unwrap().try_send(
            ChatSidebarEvent::LoadSession(session_id)
        );
        cx.notify();
    }
    
    fn on_new_chat_click(&mut self, cx: &mut Context<Self>) {
        let _ = self.events.lock().unwrap().try_send(ChatSidebarEvent::NewChat);
        cx.notify();
    }
}

impl Render for ChatSidebar {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .bg(cx.theme().sidebar)
            .h_full()
            .w_full()
            // Header with "Chats" title and new chat button
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .p_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().foreground)
                            .child("Chats")
                    )
                    .child(
                        div()
                            .size(px(24.))
                            .rounded_sm()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|s| s.bg(cx.theme().muted))
                            .child("+")
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _, cx| this.on_new_chat_click(cx))
                            )
                    )
            )
            // Chat sessions list
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .overflow_y_auto()
                    .children(
                        self.sessions.iter().map(|session| {
                            let is_selected = self.selected_session.as_ref() == Some(&session.id);
                            self.render_session_item(session, is_selected, cx)
                        })
                    )
            )
    }
    
    fn render_session_item(
        &self, 
        session: &ChatMetadata, 
        is_selected: bool,
        cx: &Context<Self>
    ) -> impl IntoElement {
        let session_id = session.id.clone();
        
        div()
            .p_2()
            .m_1()
            .rounded_md()
            .cursor_pointer()
            .bg(if is_selected {
                cx.theme().accent
            } else {
                cx.theme().background
            })
            .hover(|s| s.bg(cx.theme().muted))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .child(&session.name)
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} messages", session.message_count))
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, cx| {
                    this.on_session_click(session_id.clone(), cx)
                })
            )
    }
}
```

#### 5.2 Enhanced Root View Layout
```rust
// crates/code_assistant/src/ui/gpui/root.rs

pub struct RootView {
    // ... existing fields
    chat_sidebar: Entity<ChatSidebar>,
    chat_sidebar_collapsed: bool,
    chat_events: Arc<Mutex<async_channel::Receiver<ChatSidebarEvent>>>,
}

impl RootView {
    pub fn new(
        // ... existing parameters
        chat_sidebar: Entity<ChatSidebar>,
        chat_events: Arc<Mutex<async_channel::Receiver<ChatSidebarEvent>>>,
        // ...
    ) -> Self {
        Self {
            // ... existing fields
            chat_sidebar,
            chat_sidebar_collapsed: false,
            chat_events,
        }
    }
    
    // Update render method to include chat sidebar
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            // ... existing titlebar
            .child(
                div()
                    .size_full()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    // Left sidebar - Chat list
                    .when(!self.chat_sidebar_collapsed, |s| {
                        s.child(
                            div()
                                .flex_none()
                                .w(px(240.))
                                .h_full()
                                .child(self.chat_sidebar.clone())
                        )
                    })
                    // Center content - Messages and input
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_grow()
                            .flex_shrink()
                            .overflow_hidden()
                            // ... existing messages and input area
                    )
                    // Right sidebar - Memory (existing)
                    .when(!self.memory_collapsed, |s| {
                        s.child(
                            div()
                                .flex_none()
                                .w(px(260.))
                                .h_full()
                                .child(self.memory_view.clone())
                        )
                    })
            )
    }
}
```

### Phase 6: GPUI Integration

#### 6.1 Enhanced GPUI UserInterface
```rust
// crates/code_assistant/src/ui/gpui/mod.rs

pub struct Gpui {
    // ... existing fields
    chat_sessions: Arc<Mutex<Vec<ChatMetadata>>>,
    chat_events_sender: Arc<Mutex<async_channel::Sender<ChatSidebarEvent>>>,
    chat_events_receiver: Arc<Mutex<async_channel::Receiver<ChatSidebarEvent>>>,
}

impl Gpui {
    pub async fn load_chat_sessions(&self) -> Result<Vec<ChatMetadata>, UIError> {
        // Implementation to load chat sessions from persistence
        todo!()
    }
    
    pub async fn handle_chat_event(&self, event: ChatSidebarEvent) -> Result<(), UIError> {
        match event {
            ChatSidebarEvent::LoadSession(id) => {
                self.push_event(UiEvent::LoadChatSession { session_id: id })?;
            }
            ChatSidebarEvent::NewChat => {
                self.push_event(UiEvent::NewChatSession)?;
            }
            ChatSidebarEvent::DeleteSession(id) => {
                self.push_event(UiEvent::DeleteChatSession { session_id: id })?;
            }
            ChatSidebarEvent::RenameSession(id, name) => {
                self.push_event(UiEvent::RenameChatSession { 
                    session_id: id, 
                    new_name: name 
                })?;
            }
        }
        Ok(())
    }
}
```

#### 6.2 Enhanced UI Events
```rust
// crates/code_assistant/src/ui/gpui/ui_events.rs

#[derive(Debug, Clone)]
pub enum UiEvent {
    // ... existing events
    LoadChatSession { session_id: String },
    NewChatSession,
    DeleteChatSession { session_id: String },
    RenameChatSession { session_id: String, new_name: String },
    UpdateChatSessions { sessions: Vec<ChatMetadata> },
}
```

### Phase 7: Session ID Generation and Utilities

#### 7.1 Utility Functions
```rust
// crates/code_assistant/src/persistence.rs

use uuid::Uuid;
use chrono::{DateTime, Utc};

pub fn generate_session_id() -> String {
    format!("chat_{}", Uuid::new_v4().to_string().replace('-', "")[..12].to_lowercase())
}

pub fn truncate_task_for_name(task: &str) -> String {
    let max_len = 50;
    if task.len() <= max_len {
        task.to_string()
    } else {
        format!("{}...", &task[..max_len-3])
    }
}

pub fn format_time(time: SystemTime) -> String {
    let datetime: DateTime<Utc> = time.into();
    datetime.format("%Y-%m-%d %H:%M").to_string()
}
```

### Phase 8: Backward Compatibility

#### 8.1 Migration Support
```rust
// crates/code_assistant/src/persistence.rs

impl FileStatePersistence {
    /// Migrate old single-state format to new chat sessions
    pub fn migrate_legacy_state(&mut self) -> Result<Option<String>> {
        let legacy_path = self.root_dir.join(".code-assistant.state.json");
        if !legacy_path.exists() {
            return Ok(None);
        }
        
        // Load legacy state
        let json = std::fs::read_to_string(&legacy_path)?;
        let legacy_state: AgentState = serde_json::from_str(&json)?;
        
        // Convert to chat session
        let session = ChatSession {
            id: generate_session_id(),
            name: truncate_task_for_name(&legacy_state.task),
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
            task: legacy_state.task,
            messages: legacy_state.messages,
            tool_executions: Vec::new(), // No tool executions in legacy format
            working_memory: WorkingMemory::default(),
            init_path: None,
            initial_project: None,
        };
        
        // Save as new chat session
        self.save_chat_session(&session)?;
        
        // Remove legacy file
        std::fs::remove_file(legacy_path)?;
        
        Ok(Some(session.id))
    }
}
```

## Implementation Phases Summary

### Phase 1: Core Infrastructure (Week 1)
- [ ] Extended `ChatSession` and `SerializedToolExecution` structures
- [ ] Enhanced `StatePersistence` trait with chat session methods
- [ ] Tool execution serialization/deserialization support

### Phase 2: Persistence Layer (Week 1-2)  
- [ ] File-based chat storage implementation
- [ ] Metadata management system
- [ ] Migration from legacy single-state format

### Phase 3: Agent Integration (Week 2)
- [ ] Session management in Agent
- [ ] Chat session save/load/restore functionality
- [ ] Working memory state restoration

### Phase 4: Command Line Interface (Week 2-3)
- [ ] Extended command line arguments for chat management
- [ ] Chat listing and selection commands
- [ ] New chat session creation

### Phase 5: UI Components (Week 3-4)
- [ ] Chat sidebar component
- [ ] Session list rendering with metadata
- [ ] Chat session interaction events

### Phase 6: GPUI Integration (Week 4)
- [ ] Enhanced root view layout with chat sidebar
- [ ] Event handling for chat operations
- [ ] UI state synchronization

### Phase 7: Polish and Testing (Week 4-5)
- [ ] Comprehensive testing of all chat operations
- [ ] Error handling and edge cases
- [ ] Performance optimization for large chat histories
- [ ] Documentation and user guides

### Phase 8: Advanced Features (Future)
- [ ] Chat search and filtering
- [ ] Export/import chat sessions
- [ ] Chat session sharing
- [ ] Chat session templates

## Technical Considerations

### Performance
- **Large Chat Histories**: Implement pagination for chat lists and lazy loading for large sessions
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
- **Plugin Support**: Design chat session format to support future plugin data
- **Custom Metadata**: Allow tools to store custom metadata in sessions
- **Export Formats**: Support multiple export formats for chat sessions

This implementation plan provides a comprehensive roadmap for adding persistent chat functionality to the code-assistant project while maintaining backward compatibility and providing a rich user experience across both command-line and GUI interfaces.
