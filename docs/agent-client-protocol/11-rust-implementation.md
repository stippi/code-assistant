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
agent-client-protocol = "0.9"  # Check crates.io for latest version
```

### Feature Flags

The crate provides several feature flags:

```toml
[dependencies]
agent-client-protocol = { version = "0.9", features = ["unstable"] }
```

- `unstable` - Enables unstable features like model selection

## Core Traits

Depending on what kind of tool you're building, you'll need to implement either the `Agent` trait or the `Client` trait to define the interaction with the ACP counterpart.

### Agent Trait

Implement this trait to build an agent that responds to client requests:

```rust
use agent_client_protocol as acp;

pub trait Agent {
    // Required methods
    async fn initialize(&self, params: InitializeRequest) -> Result<InitializeResponse, Error>;
    async fn new_session(&self, params: NewSessionRequest) -> Result<NewSessionResponse, Error>;
    async fn prompt(&self, params: PromptRequest) -> Result<PromptResponse, Error>;
    async fn cancel(&self, params: CancelNotification) -> Result<(), Error>;

    // Optional methods with default implementations
    async fn authenticate(&self, params: AuthenticateRequest) -> Result<AuthenticateResponse, Error>;
    async fn load_session(&self, params: LoadSessionRequest) -> Result<LoadSessionResponse, Error>;
    async fn set_session_mode(&self, params: SetSessionModeRequest) -> Result<SetSessionModeResponse, Error>;
    
    // Extension methods
    async fn ext_method(&self, params: ExtRequest) -> Result<ExtResponse, Error>;
    async fn ext_notification(&self, params: ExtNotification) -> Result<(), Error>;
}
```

### Client Trait

Implement this trait to build a client (editor) that hosts agents:

```rust
use agent_client_protocol as acp;

pub trait Client {
    // Required methods
    async fn request_permission(&self, params: RequestPermissionRequest) -> Result<RequestPermissionResponse, Error>;

    // Optional methods for file system support
    async fn read_text_file(&self, params: ReadTextFileRequest) -> Result<ReadTextFileResponse, Error>;
    async fn write_text_file(&self, params: WriteTextFileRequest) -> Result<WriteTextFileResponse, Error>;

    // Optional methods for terminal support
    async fn create_terminal(&self, params: CreateTerminalRequest) -> Result<CreateTerminalResponse, Error>;
    async fn terminal_output(&self, params: TerminalOutputRequest) -> Result<TerminalOutputResponse, Error>;
    async fn wait_for_terminal_exit(&self, params: WaitForTerminalExitRequest) -> Result<WaitForTerminalExitResponse, Error>;
    async fn kill_terminal(&self, params: KillTerminalCommandRequest) -> Result<KillTerminalCommandResponse, Error>;
    async fn release_terminal(&self, params: ReleaseTerminalRequest) -> Result<ReleaseTerminalResponse, Error>;
}
```

## Examples

The crate includes runnable examples that demonstrate how to implement both sides:

### Agent Example

[agent.rs](https://github.com/agentclientprotocol/rust-sdk/blob/main/examples/agent.rs) - A complete example of implementing an agent server

Key points:
- Implements the `Agent` trait
- Handles JSON-RPC over stdio
- Manages sessions and conversation state
- Integrates with language models
- Executes tool calls

### Client Example

[client.rs](https://github.com/agentclientprotocol/rust-sdk/blob/main/examples/client.rs) - A complete example of implementing a client

Key points:
- Implements the `Client` trait
- Spawns agent as subprocess
- Handles stdio communication
- Manages UI for displaying agent output
- Implements file system and terminal methods

## Building an Agent

Here's a minimal agent implementation:

```rust
use agent_client_protocol as acp;
use std::cell::Cell;
use tokio::sync::{mpsc, oneshot};

struct MyAgent {
    session_update_tx: mpsc::UnboundedSender<(acp::SessionNotification, oneshot::Sender<()>)>,
    next_session_id: Cell<u64>,
}

impl acp::Agent for MyAgent {
    async fn initialize(
        &self,
        arguments: acp::InitializeRequest,
    ) -> Result<acp::InitializeResponse, acp::Error> {
        Ok(acp::InitializeResponse {
            protocol_version: acp::V1,
            agent_capabilities: acp::AgentCapabilities::default(),
            auth_methods: Vec::new(),
            agent_info: Some(acp::Implementation {
                name: "my-agent".to_string(),
                title: Some("My Agent".to_string()),
                version: "0.1.0".to_string(),
            }),
            meta: None,
        })
    }

    async fn new_session(
        &self,
        arguments: acp::NewSessionRequest,
    ) -> Result<acp::NewSessionResponse, acp::Error> {
        let session_id = self.next_session_id.get();
        self.next_session_id.set(session_id + 1);
        Ok(acp::NewSessionResponse {
            session_id: acp::SessionId(session_id.to_string().into()),
            modes: None,
            models: None,
            meta: None,
        })
    }

    async fn prompt(
        &self,
        arguments: acp::PromptRequest,
    ) -> Result<acp::PromptResponse, acp::Error> {
        // Process the prompt and send updates via session_update_tx
        // ...
        
        Ok(acp::PromptResponse {
            stop_reason: acp::StopReason::EndTurn,
            meta: None,
        })
    }

    async fn cancel(&self, args: acp::CancelNotification) -> Result<(), acp::Error> {
        // Handle cancellation
        Ok(())
    }
}
```

## Running with AgentSideConnection

The `AgentSideConnection` handles the JSON-RPC transport over stdio:

```rust
use agent_client_protocol as acp;
use tokio_util::compat::{TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _};

#[tokio::main(flavor = "current_thread")]
async fn main() -> acp::Result<()> {
    let outgoing = tokio::io::stdout().compat_write();
    let incoming = tokio::io::stdin().compat();

    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async move {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            
            let (conn, handle_io) =
                acp::AgentSideConnection::new(MyAgent::new(tx), outgoing, incoming, |fut| {
                    tokio::task::spawn_local(fut);
                });
            
            // Background task to send session notifications
            tokio::task::spawn_local(async move {
                while let Some((notification, tx)) = rx.recv().await {
                    let result = conn.session_notification(notification).await;
                    if let Err(e) = result {
                        log::error!("{e}");
                        break;
                    }
                    tx.send(()).ok();
                }
            });
            
            handle_io.await
        })
        .await
}
```

## Sending Notifications

Agents send notifications to clients for streaming updates:

```rust
// Send a session update notification
let notification = acp::SessionNotification {
    session_id: session_id.clone(),
    update: acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk {
        content: acp::ContentBlock::Text(acp::TextContent {
            text: "Thinking...".to_string(),
            annotations: None,
            meta: None,
        }),
        meta: None,
    }),
    meta: None,
};

conn.session_notification(notification).await?;
```

## Error Handling

The crate provides an `Error` type for protocol errors:

```rust
use agent_client_protocol::Error;

// Create standard errors
let error = Error::internal_error();
let error = Error::invalid_params();
let error = Error::method_not_found();

// Errors can be returned from trait methods
async fn my_method(&self, params: Params) -> Result<Response, Error> {
    if !valid {
        return Err(Error::invalid_params());
    }
    Ok(response)
}
```

## Type Safety

The crate provides strong typing for all protocol types:

- `SessionId` - Unique session identifier
- `ToolCallId` - Unique tool call identifier
- `ContentBlock` - Union type for different content types
- `StopReason` - Enumeration of turn completion reasons
- `ToolKind` - Enumeration of tool categories
- `SessionUpdate` - Different types of session updates
- And many more...

## Documentation

Full API documentation is available on [docs.rs](https://docs.rs/agent-client-protocol/latest/agent_client_protocol/).

## Resources

- **Crate**: https://crates.io/crates/agent-client-protocol
- **Documentation**: https://docs.rs/agent-client-protocol
- **Examples**: https://github.com/agentclientprotocol/rust-sdk/tree/main/examples
- **GitHub**: https://github.com/agentclientprotocol/agent-client-protocol
- **Rust SDK**: https://github.com/agentclientprotocol/rust-sdk
