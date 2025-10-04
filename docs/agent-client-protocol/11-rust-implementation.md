# Rust Implementation

The `agent-client-protocol` Rust crate provides implementations of both sides of the Agent Client Protocol that you can use to build your own agent server or client.

## Installation

Add the crate as a dependency to your project's `Cargo.toml`:

```bash
cargo add agent-client-protocol
```

Or add it manually:

```toml
[dependencies]
agent-client-protocol = "0.1"  # Check crates.io for latest version
```

## Core Traits

Depending on what kind of tool you're building, you'll need to implement either the `Agent` trait or the `Client` trait to define the interaction with the ACP counterpart.

### Agent Trait

Implement this trait to build an agent that responds to client requests:

```rust
use agent_client_protocol::Agent;

pub trait Agent {
    // Required methods
    async fn initialize(&mut self, params: InitializeRequest) -> Result<InitializeResponse>;
    async fn new_session(&mut self, params: NewSessionRequest) -> Result<NewSessionResponse>;
    async fn prompt(&mut self, params: PromptRequest) -> Result<PromptResponse>;
    
    // Optional methods with default implementations
    async fn authenticate(&mut self, params: AuthenticateRequest) -> Result<AuthenticateResponse> {
        // Default implementation
    }
    
    async fn load_session(&mut self, params: LoadSessionRequest) -> Result<LoadSessionResponse> {
        // Default implementation - returns unsupported
    }
    
    async fn set_session_mode(&mut self, params: SetSessionModeRequest) -> Result<SetSessionModeResponse> {
        // Default implementation
    }
}
```

### Client Trait

Implement this trait to build a client (editor) that hosts agents:

```rust
use agent_client_protocol::Client;

pub trait Client {
    // Required methods
    async fn request_permission(&mut self, params: RequestPermissionRequest) -> Result<RequestPermissionResponse>;
    
    // Optional methods for file system support
    async fn read_text_file(&mut self, params: ReadTextFileRequest) -> Result<ReadTextFileResponse> {
        // Default implementation - returns unsupported
    }
    
    async fn write_text_file(&mut self, params: WriteTextFileRequest) -> Result<WriteTextFileResponse> {
        // Default implementation - returns unsupported
    }
    
    // Optional methods for terminal support
    async fn create_terminal(&mut self, params: CreateTerminalRequest) -> Result<CreateTerminalResponse> {
        // Default implementation - returns unsupported
    }
    
    async fn terminal_output(&mut self, params: TerminalOutputRequest) -> Result<TerminalOutputResponse> {
        // Default implementation - returns unsupported
    }
    
    async fn wait_for_terminal_exit(&mut self, params: WaitForTerminalExitRequest) -> Result<WaitForTerminalExitResponse> {
        // Default implementation - returns unsupported
    }
    
    async fn kill_terminal(&mut self, params: KillTerminalRequest) -> Result<KillTerminalResponse> {
        // Default implementation - returns unsupported
    }
    
    async fn release_terminal(&mut self, params: ReleaseTerminalRequest) -> Result<ReleaseTerminalResponse> {
        // Default implementation - returns unsupported
    }
}
```

## Examples

The crate includes runnable examples that demonstrate how to implement both sides:

### Agent Example

[agent.rs](https://github.com/zed-industries/agent-client-protocol/blob/main/rust/examples/agent.rs) - A complete example of implementing an agent server

Key points:
- Implements the `Agent` trait
- Handles JSON-RPC over stdio
- Manages sessions and conversation state
- Integrates with language models
- Executes tool calls

### Client Example

[client.rs](https://github.com/zed-industries/agent-client-protocol/blob/main/rust/examples/client.rs) - A complete example of implementing a client

Key points:
- Implements the `Client` trait
- Spawns agent as subprocess
- Handles stdio communication
- Manages UI for displaying agent output
- Implements file system and terminal methods

## Building an Agent

Here's a minimal agent implementation:

```rust
use agent_client_protocol::{Agent, InitializeRequest, InitializeResponse, 
                             NewSessionRequest, NewSessionResponse,
                             PromptRequest, PromptResponse, StopReason};
use std::sync::Arc;

struct MyAgent {
    sessions: HashMap<SessionId, SessionState>,
}

impl Agent for MyAgent {
    async fn initialize(&mut self, params: InitializeRequest) -> Result<InitializeResponse> {
        Ok(InitializeResponse {
            protocol_version: params.protocol_version,
            agent_capabilities: AgentCapabilities {
                load_session: false,
                prompt_capabilities: PromptCapabilities {
                    image: true,
                    audio: false,
                    embedded_context: true,
                },
                mcp_capabilities: McpCapabilities {
                    http: true,
                    sse: false,
                },
            },
            auth_methods: vec![],
            _meta: None,
        })
    }
    
    async fn new_session(&mut self, params: NewSessionRequest) -> Result<NewSessionResponse> {
        let session_id = SessionId(Arc::from(format!("sess_{}", uuid::Uuid::new_v4())));
        
        // Initialize session state
        let state = SessionState::new(params.cwd, params.mcp_servers);
        self.sessions.insert(session_id.clone(), state);
        
        Ok(NewSessionResponse {
            session_id,
            modes: None,
            models: None,
            _meta: None,
        })
    }
    
    async fn prompt(&mut self, params: PromptRequest) -> Result<PromptResponse> {
        let session = self.sessions.get_mut(&params.session_id)
            .ok_or_else(|| Error::SessionNotFound)?;
        
        // Process the prompt with your LLM
        // Send updates via session/update notifications
        // Handle tool calls
        // ...
        
        Ok(PromptResponse {
            stop_reason: StopReason::EndTurn,
            _meta: None,
        })
    }
}
```

## Building a Client

Here's a minimal client implementation:

```rust
use agent_client_protocol::{Client, RequestPermissionRequest, RequestPermissionResponse,
                             RequestPermissionOutcome, PermissionOptionId};

struct MyClient {
    // UI state, file system access, etc.
}

impl Client for MyClient {
    async fn request_permission(&mut self, params: RequestPermissionRequest) -> Result<RequestPermissionResponse> {
        // Show UI to user with the provided options
        // Wait for user selection
        let selected_option_id = self.show_permission_dialog(&params).await?;
        
        Ok(RequestPermissionResponse {
            outcome: RequestPermissionOutcome::Selected {
                option_id: selected_option_id,
            },
            _meta: None,
        })
    }
    
    async fn read_text_file(&mut self, params: ReadTextFileRequest) -> Result<ReadTextFileResponse> {
        // Read file from editor state (including unsaved changes)
        let content = self.read_editor_buffer(&params.path).await?;
        
        Ok(ReadTextFileResponse {
            content,
            _meta: None,
        })
    }
    
    async fn write_text_file(&mut self, params: WriteTextFileRequest) -> Result<WriteTextFileResponse> {
        // Write file through editor
        self.write_editor_buffer(&params.path, &params.content).await?;
        
        Ok(WriteTextFileResponse {
            _meta: None,
        })
    }
}
```

## JSON-RPC Transport

The protocol uses JSON-RPC 2.0 over stdio. The crate provides utilities for handling this:

```rust
use agent_client_protocol::transport::{StdioTransport, Transport};

// For agents (server side)
let mut transport = StdioTransport::new();
let mut agent = MyAgent::new();

loop {
    let request = transport.receive().await?;
    let response = agent.handle_request(request).await?;
    transport.send(response).await?;
}

// For clients (spawning agent subprocess)
let mut child = Command::new("path/to/agent")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .spawn()?;

let mut transport = StdioTransport::from_child(&mut child);
```

## Sending Notifications

Agents send notifications to clients for streaming updates:

```rust
// Send a session update notification
transport.send_notification("session/update", SessionNotification {
    session_id: session_id.clone(),
    update: SessionUpdate::AgentMessageChunk {
        content: ContentBlock::Text {
            text: "Thinking...".to_string(),
            annotations: None,
            _meta: None,
        },
    },
    _meta: None,
}).await?;
```

## Error Handling

The crate provides an `Error` type for protocol errors:

```rust
use agent_client_protocol::Error;

match result {
    Ok(response) => // handle success,
    Err(Error::SessionNotFound) => // session doesn't exist,
    Err(Error::MethodNotSupported) => // capability not available,
    Err(Error::AuthRequired) => // authentication needed,
    Err(e) => // other error,
}
```

## Type Safety

The crate provides strong typing for all protocol types:

- `SessionId` - Unique session identifier
- `ToolCallId` - Unique tool call identifier
- `ContentBlock` - Union type for different content types
- `StopReason` - Enumeration of turn completion reasons
- `ToolKind` - Enumeration of tool categories
- And many more...

## Documentation

Full API documentation is available on [docs.rs](https://docs.rs/agent-client-protocol/latest/agent_client_protocol/).

## Real-World Usage

The `agent-client-protocol` crate powers the integration with external agents in the [Zed](https://zed.dev) editor.

You can study Zed's source code to see a production implementation of the client side.

## Resources

- **Crate**: https://crates.io/crates/agent-client-protocol
- **Documentation**: https://docs.rs/agent-client-protocol
- **Examples**: https://github.com/zed-industries/agent-client-protocol/tree/main/rust/examples
- **GitHub**: https://github.com/zed-industries/agent-client-protocol
