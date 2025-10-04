# ACP Integration Implementation Plan

## Overview

This document outlines the plan to integrate Agent Client Protocol (ACP) support into code-assistant as an additional run mode alongside the existing terminal UI, GPUI, and MCP server modes.

## Background

The Agent Client Protocol standardizes communication between code editors (clients) and AI coding agents. By implementing ACP, code-assistant can become a standardized agent that works with any ACP-compatible editor, including Zed.

### Current Architecture

code-assistant currently supports three run modes:
1. **Terminal UI** (`code-assistant --task "..."`) - Interactive terminal interface
2. **GPUI** (`code-assistant --ui`) - Modern graphical interface
3. **MCP Server** (`code-assistant server`) - Model Context Protocol server for Claude Desktop

The architecture has:
- `Agent` struct in `agent/runner.rs` - Core agent logic with LLM interaction
- `app/` module - Different run modes (terminal, gpui, server)
- `mcp/` module - MCP server implementation (handler, resources, types)
- `tools/` module - Tool implementations and parsers
- `session/` module - Session management

## Implementation Plan

### Phase 1: Add ACP Crate Dependency

**Files to modify:**
- `Cargo.toml` (workspace root)
- `crates/code_assistant/Cargo.toml`

**Changes:**
```toml
[dependencies]
agent-client-protocol = "0.1"  # Check crates.io for latest version
```

### Phase 2: Create ACP Module Structure

**New directory structure:**
```
crates/code_assistant/src/acp/
├── mod.rs           # Module exports
├── server.rs        # Main ACP server (similar to mcp/server.rs)
├── agent_impl.rs    # Implementation of Agent trait
├── types.rs         # ACP-specific types and conversions
└── session.rs       # ACP session management
```

### Phase 3: Implement ACP Server Entry Point

**File:** `crates/code_assistant/src/acp/server.rs`

Similar to MCP server implementation:
```rust
pub struct ACPServer {
    agent_impl: AgentImpl,
}

impl ACPServer {
    pub fn new() -> Result<Self> {
        Ok(Self {
            agent_impl: AgentImpl::new()?,
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        // JSON-RPC over stdio
        // Read from stdin, write to stdout
        // Handle initialize, session/new, session/prompt, etc.
    }
}
```

Key responsibilities:
- Read JSON-RPC messages from stdin
- Dispatch to agent implementation
- Send responses and notifications to stdout
- Handle protocol initialization and version negotiation

### Phase 4: Implement Agent Trait

**File:** `crates/code_assistant/src/acp/agent_impl.rs`

Implement the `agent_client_protocol::Agent` trait:

```rust
use agent_client_protocol::{
    Agent, InitializeRequest, InitializeResponse,
    NewSessionRequest, NewSessionResponse,
    PromptRequest, PromptResponse, StopReason,
    SessionUpdate, SessionNotification,
};

pub struct AgentImpl {
    // Similar to current Agent struct
    sessions: HashMap<SessionId, SessionState>,
    project_manager: Box<dyn ProjectManager>,
    // ... other fields
}

impl Agent for AgentImpl {
    async fn initialize(&mut self, params: InitializeRequest) -> Result<InitializeResponse> {
        // Return capabilities:
        // - loadSession: false (initially)
        // - promptCapabilities: { image: false, audio: false, embeddedContext: true }
        // - mcpCapabilities: { http: false, sse: false }
    }

    async fn new_session(&mut self, params: NewSessionRequest) -> Result<NewSessionResponse> {
        // Create a new agent instance
        // Set up working directory from params.cwd
        // Connect to MCP servers if provided
        // Return unique session ID
    }

    async fn prompt(&mut self, params: PromptRequest) -> Result<PromptResponse> {
        // Main interaction loop
        // Process user prompt
        // Stream updates via session/update notifications
        // Execute tool calls
        // Return stop reason
    }
}
```

### Phase 5: Session Management

**File:** `crates/code_assistant/src/acp/session.rs`

Each ACP session needs to maintain:
```rust
struct ACPSession {
    session_id: SessionId,
    agent: crate::agent::Agent,  // Reuse existing Agent
    cwd: PathBuf,
    mcp_servers: Vec<McpServer>,
    transport: Arc<Mutex<StdioTransport>>,  // For sending notifications
}
```

Key features:
- Map ACP SessionId to internal Agent instance
- Handle session lifecycle (create, load if supported, cleanup)
- Bridge between ACP protocol and internal Agent

### Phase 6: Type Conversions

**File:** `crates/code_assistant/src/acp/types.rs`

Implement conversions between:

**ACP → Internal:**
- `ContentBlock` → internal content representation
- `PromptRequest` → internal prompt format
- `StopReason` → internal completion status

**Internal → ACP:**
- Tool calls → `SessionUpdate::ToolCall`
- Assistant messages → `SessionUpdate::AgentMessageChunk`
- File diffs → `ToolCallContent::Diff`
- Terminal output → `ToolCallContent::Terminal`

### Phase 7: Streaming Updates

Implement streaming of agent activity to client:

```rust
impl AgentImpl {
    async fn send_update(&self, session_id: &SessionId, update: SessionUpdate) -> Result<()> {
        // Send session/update notification via transport
        self.transport.send_notification("session/update", SessionNotification {
            session_id: session_id.clone(),
            update,
            _meta: None,
        }).await
    }

    async fn stream_agent_message(&self, session_id: &SessionId, text: &str) -> Result<()> {
        self.send_update(session_id, SessionUpdate::AgentMessageChunk {
            content: ContentBlock::Text {
                text: text.to_string(),
                annotations: None,
                _meta: None,
            },
        }).await
    }

    async fn report_tool_call(&self, session_id: &SessionId, tool_call: ToolCall) -> Result<()> {
        self.send_update(session_id, SessionUpdate::ToolCall {
            tool_call_id: tool_call.tool_call_id,
            title: tool_call.title,
            kind: tool_call.kind,
            status: tool_call.status,
            // ... other fields
        }).await
    }
}
```

### Phase 8: Tool Call Integration

Map existing tools to ACP tool calls:

**Read operations** → `ToolKind::Read`
- `read_file`
- `list_files`

**Edit operations** → `ToolKind::Edit`
- `write_file`
- `replace_in_file`
- `edit` (with diff output)

**Execute operations** → `ToolKind::Execute`
- `execute_command`

**Search operations** → `ToolKind::Search`
- `web_search`
- `glob_files`
- `search_files`

**Other operations** → `ToolKind::Other`
- `perplexity_ask`
- `delete_files`
- etc.

### Phase 9: Client Capabilities Support

Optionally implement client method calls when needed:

**File System:**
```rust
// Call client's fs/read_text_file when we need file access
async fn read_file_from_client(&self, session_id: &SessionId, path: &str) -> Result<String> {
    let request = ReadTextFileRequest {
        session_id: session_id.clone(),
        path: path.to_string(),
        line: None,
        limit: None,
        _meta: None,
    };

    let response = self.transport.call("fs/read_text_file", request).await?;
    Ok(response.content)
}
```

**Terminals:**
```rust
// Create terminal on client side for command execution
async fn create_client_terminal(&self, session_id: &SessionId, command: &str, args: &[String]) -> Result<String> {
    let request = CreateTerminalRequest {
        session_id: session_id.clone(),
        command: command.to_string(),
        args: args.to_vec(),
        env: vec![],
        cwd: None,
        output_byte_limit: Some(1048576),
        _meta: None,
    };

    let response = self.transport.call("terminal/create", request).await?;
    Ok(response.terminal_id)
}
```

### Phase 10: CLI Integration

**File:** `crates/code_assistant/src/cli.rs`

Add new mode:
```rust
#[derive(Subcommand, Debug)]
pub enum Mode {
    /// Run as MCP server
    Server {
        #[arg(short, long)]
        verbose: bool,
    },

    /// Run as ACP agent
    #[command(name = "acp")]
    Acp {
        #[arg(short, long)]
        verbose: bool,
    },
}
```

**File:** `crates/code_assistant/src/main.rs`

Add handler:
```rust
match args.mode {
    Some(Mode::Server { verbose }) => app::server::run(verbose).await,
    Some(Mode::Acp { verbose }) => app::acp::run(verbose).await,
    None => {
        // Existing terminal/GPUI mode logic
    }
}
```

**File:** `crates/code_assistant/src/app/mod.rs`

Add module:
```rust
pub mod acp;
pub mod gpui;
pub mod server;
pub mod terminal;
```

**File:** `crates/code_assistant/src/app/acp.rs`

Entry point:
```rust
use crate::acp::ACPServer;
use crate::logging::setup_logging;
use anyhow::Result;

pub async fn run(verbose: bool) -> Result<()> {
    setup_logging(if verbose { 1 } else { 0 }, false);

    let mut server = ACPServer::new()?;
    server.run().await
}
```

### Phase 11: Permission Handling

Implement `session/request_permission` handling for sensitive operations:

```rust
async fn request_permission(&self, session_id: &SessionId, tool_call: ToolCallUpdate) -> Result<PermissionResponse> {
    let request = RequestPermissionRequest {
        session_id: session_id.clone(),
        tool_call,
        options: vec![
            PermissionOption {
                option_id: PermissionOptionId::from("allow-once"),
                name: "Allow once".to_string(),
                kind: PermissionOptionKind::AllowOnce,
                _meta: None,
            },
            PermissionOption {
                option_id: PermissionOptionId::from("reject-once"),
                name: "Reject".to_string(),
                kind: PermissionOptionKind::RejectOnce,
                _meta: None,
            },
        ],
        _meta: None,
    };

    self.transport.call("session/request_permission", request).await
}
```

### Phase 12: Error Handling and Cancellation

Implement proper cancellation support:

```rust
// Handle session/cancel notifications
async fn handle_cancel(&mut self, session_id: &SessionId) {
    if let Some(session) = self.sessions.get_mut(session_id) {
        // Stop LLM requests
        session.agent.cancel_current_operation().await;

        // Respond to pending prompt with cancelled stop reason
        // ...
    }
}
```

### Phase 13: Testing

Create test suite:

**File:** `crates/code_assistant/src/acp/tests.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_initialize() {
        let mut agent = AgentImpl::new().unwrap();
        let request = InitializeRequest {
            protocol_version: 1,
            client_capabilities: ClientCapabilities::default(),
            _meta: None,
        };

        let response = agent.initialize(request).await.unwrap();
        assert_eq!(response.protocol_version, 1);
        assert!(response.agent_capabilities.prompt_capabilities.embedded_context);
    }

    #[tokio::test]
    async fn test_session_lifecycle() {
        // Test session creation, prompt, and cleanup
    }

    #[tokio::test]
    async fn test_tool_execution() {
        // Test tool call reporting and execution
    }
}
```

## Architecture Diagram

```
┌─────────────────────────────────────────────────┐
│              ACP Client (Editor)                │
│                  (e.g., Zed)                    │
└───────────────────┬─────────────────────────────┘
                    │ JSON-RPC over stdio
                    │
┌───────────────────▼─────────────────────────────┐
│            ACPServer (server.rs)                │
│  - Read/Write JSON-RPC messages                 │
│  - Protocol version negotiation                 │
│  - Session management                           │
└───────────────────┬─────────────────────────────┘
                    │
┌───────────────────▼─────────────────────────────┐
│         AgentImpl (agent_impl.rs)               │
│  Implements agent_client_protocol::Agent        │
│  - initialize()                                 │
│  - new_session()                                │
│  - prompt()                                     │
└───────────────────┬─────────────────────────────┘
                    │
        ┌───────────┴───────────┐
        │                       │
┌───────▼────────┐   ┌─────────▼──────────┐
│  ACPSession    │   │  Type Conversions  │
│  (session.rs)  │   │    (types.rs)      │
└───────┬────────┘   └────────────────────┘
        │
        │ Wraps and delegates to
        │
┌───────▼────────────────────────────────────────┐
│          agent::Agent (runner.rs)              │
│  Existing agent implementation                 │
│  - LLM interaction                             │
│  - Tool execution                              │
│  - Working memory                              │
└────────────────────────────────────────────────┘
```

## Benefits

1. **Editor Interoperability**: Works with any ACP-compatible editor (Zed, etc.)
2. **Reuses Existing Logic**: Leverages current Agent implementation
3. **Consistent Experience**: Same capabilities across terminal, GPUI, MCP, and ACP modes
4. **Standardized Protocol**: Uses industry-standard communication patterns

## Future Enhancements

1. **Load Session Support**: Implement session persistence and loading
2. **Session Modes**: Support different operating modes (ask, code, architect)
3. **Model Selection**: Allow clients to switch between different LLM models
4. **Advanced Capabilities**: Image support, audio transcription
5. **HTTP/SSE MCP**: Support HTTP and SSE transports for MCP servers

## Migration Path

1. Start with minimal implementation (initialize, new_session, prompt)
2. Add tool call reporting and streaming
3. Implement client capabilities (file system, terminals) as needed
4. Add permission handling
5. Implement cancellation support
6. Add session persistence (load_session capability)
7. Add advanced features (modes, model selection)

## Testing Strategy

1. **Unit Tests**: Test individual components (type conversions, session management)
2. **Integration Tests**: Test full protocol flow with mock client
3. **Manual Testing**: Test with Zed or custom ACP client
4. **Compatibility Tests**: Ensure protocol compliance with spec

## Documentation

1. Update README.md with ACP mode usage
2. Add ACP configuration examples
3. Document how to use code-assistant as ACP agent in Zed
4. Add troubleshooting guide for ACP integration

## References

- [ACP Documentation](./README.md)
- [ACP Rust Crate](https://docs.rs/agent-client-protocol)
- [ACP GitHub Examples](https://github.com/zed-industries/agent-client-protocol)
- [code-assistant Architecture](../../README.md)
