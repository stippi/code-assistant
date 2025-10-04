# ACP Integration Implementation Plan (Updated)

## Overview

This plan implements Agent Client Protocol (ACP) support as a UI implementation in code-assistant. The key insight is that ACP's session/update events map directly to DisplayFragment events used in GPUI, making the implementation straightforward.

## Core Insight: ACP as a UI

ACP should be implemented as another `UserInterface` implementation, similar to GPUI and Terminal UI:

- **Session Loading**: Convert message history to DisplayFragments and send as session/update events
- **Prompt Handling**: On `session/prompt`, start the agent loop until completion
- **Streaming Updates**: DisplayFragments → session/update notifications in real-time
- **Tool Execution**: Same tool implementations, just different event format

The `agent-client-protocol` Rust crate handles all JSON-RPC transport details.

## Architecture

```
┌─────────────────────────────────────────────────┐
│              ACP Client (Editor)                │
│                  (e.g., Zed)                    │
└───────────────────┬─────────────────────────────┘
                    │ JSON-RPC over stdio
                    │ (agent-client-protocol crate)
┌───────────────────▼─────────────────────────────┐
│         ACPAgentImpl (acp/agent.rs)             │
│  Implements agent_client_protocol::Agent        │
│  - initialize()    → capabilities               │
│  - new_session()   → create SessionInstance     │
│  - prompt()        → start agent loop           │
│  - load_session()  → load from persistence      │
└───────────────────┬─────────────────────────────┘
                    │
        ┌───────────┴───────────┐
        │                       │
┌───────▼────────┐   ┌─────────▼──────────┐
│  ACPUserUI     │   │  SessionManager    │
│  (acp/ui.rs)   │   │  (existing)        │
│  Implements    │   │  - Session state   │
│  UserInterface │   │  - Message history │
│  - send_event()│   │  - Agent lifecycle │
│  - converts    │   └────────────────────┘
│    DisplayFrag │
│    to session/ │
│    update      │
└────────────────┘
```

## Implementation Steps

### 1. Add ACP Dependency

**File**: `crates/code_assistant/Cargo.toml`

```toml
[dependencies]
agent-client-protocol = "0.1"
```

### 2. Create ACP Module

**Directory**: `crates/code_assistant/src/acp/`

Files:
- `mod.rs` - Module exports
- `agent.rs` - Main Agent trait implementation  
- `ui.rs` - UserInterface implementation that sends session/update events
- `types.rs` - Type conversions between DisplayFragment and ACP types

### 3. Implement ACPUserUI

**File**: `crates/code_assistant/src/acp/ui.rs`

This is the core innovation - a UserInterface that converts DisplayFragments to ACP session/update:

```rust
pub struct ACPUserUI {
    session_id: SessionId,
    connection: Arc<Mutex<AgentSideConnection>>,
}

#[async_trait]
impl UserInterface for ACPUserUI {
    async fn send_event(&self, event: UiEvent) -> Result<(), UIError> {
        match event {
            UiEvent::AppendToTextBlock { content } => {
                self.send_session_update(SessionUpdate::AgentMessageChunk {
                    content: ContentBlock::Text { text: content, .. },
                }).await?;
            }
            UiEvent::StartTool { name, id } => {
                self.send_session_update(SessionUpdate::ToolCall {
                    tool_call_id,
                    title: name,
                    kind: map_to_tool_kind(&name),
                    status: ToolCallStatus::Executing,
                    // ...
                }).await?;
            }
            // ... other event types
        }
    }
    
    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError> {
        // Convert DisplayFragment to ACP ContentBlock
        let content = match fragment {
            DisplayFragment::PlainText(text) => {
                ContentBlock::Text { text, .. }
            }
            DisplayFragment::ThinkingText(text) => {
                ContentBlock::Text { 
                    text,
                    annotations: Some(vec![Annotation::Thought]),
                    ..
                }
            }
            // ... other fragment types
        };
        
        self.send_session_update(SessionUpdate::AgentMessageChunk { content })?;
    }
}
```

### 4. Implement ACPAgentImpl

**File**: `crates/code_assistant/src/acp/agent.rs`

```rust
pub struct ACPAgentImpl {
    session_manager: Arc<Mutex<SessionManager>>,
    agent_config: AgentConfig,
    llm_config: LLMClientConfig,
}

#[async_trait(?Send)]
impl Agent for ACPAgentImpl {
    async fn initialize(&self, req: InitializeRequest) 
        -> Result<InitializeResponse, Error> {
        Ok(InitializeResponse {
            protocol_version: acp::V1,
            agent_capabilities: AgentCapabilities {
                load_session: true,  // We support session loading
                prompt_capabilities: PromptCapabilities {
                    image: true,
                    audio: false,
                    embedded_context: true,
                },
                mcp_capabilities: McpCapabilities::default(),
            },
            auth_methods: vec![],
            meta: None,
        })
    }

    async fn new_session(&self, req: NewSessionRequest) 
        -> Result<NewSessionResponse, Error> {
        let session_id = {
            let mut manager = self.session_manager.lock().await;
            manager.create_session_with_config(None, Some(llm_config))?
        };
        
        Ok(NewSessionResponse {
            session_id: SessionId(session_id.into()),
            modes: None,
            models: None,
            meta: None,
        })
    }

    async fn load_session(&self, req: LoadSessionRequest) 
        -> Result<LoadSessionResponse, Error> {
        let messages = {
            let mut manager = self.session_manager.lock().await;
            manager.load_session(&req.session_id.0)?
        };
        
        // Send session/update events to replay message history
        self.replay_session_history(&req.session_id, messages).await?;
        
        Ok(LoadSessionResponse {
            modes: None,
            models: None,
            meta: None,
        })
    }

    async fn prompt(&self, req: PromptRequest) 
        -> Result<PromptResponse, Error> {
        // Get session instance
        let session_instance = {
            let manager = self.session_manager.lock().await;
            manager.get_session(&req.session_id.0)?
        };
        
        // Create ACPUserUI for this session
        let ui = Arc::new(ACPUserUI::new(
            req.session_id.clone(),
            self.connection.clone(),
        ));
        
        // Start agent with the prompt
        {
            let mut manager = self.session_manager.lock().await;
            let llm_client = create_llm_client(self.llm_config.clone()).await?;
            let project_manager = Box::new(DefaultProjectManager::new());
            let command_executor = Box::new(DefaultCommandExecutor);
            
            manager.start_agent_for_message(
                &req.session_id.0,
                convert_prompt_to_content_blocks(req.prompt),
                llm_client,
                project_manager,
                command_executor,
                ui,
            ).await?;
        }
        
        // Wait for agent to complete
        // Agent will send session/update events via ACPUserUI
        session_instance.wait_for_completion().await?;
        
        Ok(PromptResponse {
            stop_reason: StopReason::EndTurn,
            meta: None,
        })
    }
}
```

### 5. Type Conversions

**File**: `crates/code_assistant/src/acp/types.rs`

Map between internal types and ACP types:

```rust
// DisplayFragment → ACP ContentBlock
pub fn fragment_to_content_block(fragment: &DisplayFragment) -> ContentBlock {
    match fragment {
        DisplayFragment::PlainText(text) => ContentBlock::Text {
            text: text.clone(),
            annotations: None,
            meta: None,
        },
        DisplayFragment::ThinkingText(text) => ContentBlock::Text {
            text: text.clone(),
            annotations: Some(vec![Annotation::Thought]),
            meta: None,
        },
        // ... more mappings
    }
}

// Tool name → ACP ToolKind
pub fn map_tool_kind(tool_name: &str) -> ToolKind {
    match tool_name {
        "read_files" | "list_files" => ToolKind::Read,
        "write_file" | "edit" | "replace_in_file" => ToolKind::Edit,
        "execute_command" => ToolKind::Execute,
        "web_search" | "glob_files" | "search_files" => ToolKind::Search,
        _ => ToolKind::Other,
    }
}
```

### 6. Add CLI Mode

**File**: `crates/code_assistant/src/cli.rs`

```rust
#[derive(Subcommand, Debug)]
pub enum Mode {
    Server { verbose: bool },
    
    /// Run as ACP agent
    Acp { 
        #[arg(short, long)]
        verbose: bool 
    },
}
```

**File**: `crates/code_assistant/src/main.rs`

```rust
match args.mode {
    Some(Mode::Server { verbose }) => app::server::run(verbose).await,
    Some(Mode::Acp { verbose }) => app::acp::run(verbose, config).await,
    None => { /* existing UI modes */ }
}
```

**File**: `crates/code_assistant/src/app/acp.rs`

```rust
pub async fn run(verbose: bool, config: AgentRunConfig) -> Result<()> {
    setup_logging(if verbose { 1 } else { 0 }, false);
    
    let agent_config = AgentConfig {
        tool_syntax: config.tool_syntax,
        init_path: Some(config.path.canonicalize()?),
        initial_project: String::new(),
        use_diff_blocks: config.use_diff_format,
    };
    
    let llm_config = LLMClientConfig {
        provider: config.provider,
        model: config.model,
        base_url: config.base_url,
        aicore_config: config.aicore_config,
        num_ctx: config.num_ctx,
        record_path: config.record,
        playback_path: config.playback,
        fast_playback: config.fast_playback,
    };
    
    let persistence = FileSessionPersistence::new();
    let session_manager = Arc::new(Mutex::new(
        SessionManager::new(persistence, agent_config)
    ));
    
    let agent = ACPAgentImpl::new(session_manager, llm_config);
    
    // Use agent-client-protocol crate to handle stdio transport
    let outgoing = tokio::io::stdout().compat_write();
    let incoming = tokio::io::stdin().compat();
    
    let local_set = tokio::task::LocalSet::new();
    local_set.run_until(async move {
        let (conn, handle_io) = AgentSideConnection::new(
            agent,
            outgoing,
            incoming,
            |fut| { tokio::task::spawn_local(fut); }
        );
        
        handle_io.await
    }).await
}
```

### 7. Session History Replay

When loading a session, convert all messages to DisplayFragments and send as session/update:

```rust
impl ACPAgentImpl {
    async fn replay_session_history(
        &self,
        session_id: &SessionId,
        messages: Vec<Message>,
    ) -> Result<(), Error> {
        let session = {
            let manager = self.session_manager.lock().await;
            manager.get_session(&session_id.0)?
        };
        
        // Use stream processor to extract DisplayFragments
        let mut processor = create_stream_processor(
            session.session.tool_syntax,
            Arc::new(NoOpUI),  // Don't send to UI yet
            0,
        );
        
        for message in messages {
            let fragments = processor.extract_fragments_from_message(&message)?;
            
            for fragment in fragments {
                let content = fragment_to_content_block(&fragment);
                self.send_session_update(
                    session_id.clone(),
                    SessionUpdate::AgentMessageChunk { content }
                ).await?;
            }
        }
        
        Ok(())
    }
}
```

## Benefits of This Approach

1. **Minimal Code**: Reuses existing SessionManager, Agent, and DisplayFragment infrastructure
2. **Natural Mapping**: DisplayFragments → session/update events is straightforward
3. **Consistent Behavior**: Same agent logic across all UI modes
4. **Clean Separation**: ACP is just another UI consumer of the agent

## Testing

1. Test with example ACP client from the crate
2. Test with Zed editor
3. Verify session loading replays history correctly
4. Verify streaming updates work in real-time

## Future Enhancements

1. **Permission Handling**: Implement request_permission for sensitive operations
2. **File System Methods**: Implement fs/read_text_file, fs/write_text_file
3. **Terminal Methods**: Implement terminal creation and management
4. **Session Modes**: Support different operating modes (ask, code, architect)
5. **Model Selection**: Allow clients to switch models
