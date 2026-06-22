# Rust Implementation

The `agent-client-protocol` Rust crate provides implementations of both sides of
the Agent Client Protocol that you can use to build your own agent server or
client.

> **SDK version note.** The crate was substantially redesigned at `0.11.0`. The
> old `AgentSideConnection` + `impl acp::Agent` trait model (used up to `0.10.4`)
> was replaced by a Role/Component **connection builder** model. This document
> describes the current API (`0.14.x`, the version Zed ships); code-assistant's
> `ui_acp` crate targets `0.14.0`.

## Installation

Add the crate as a dependency to your project's `Cargo.toml`:

```bash
cargo add agent-client-protocol
```

Or add it manually:

```toml
[dependencies]
agent-client-protocol = "0.14"  # Check crates.io for latest version
```

### Feature Flags

The crate exposes a number of `unstable_*` feature flags (and an `unstable`
umbrella that turns them all on). Everything code-assistant needs is in the
**stable** surface, so we compile without `unstable`:

- Session **config options** (`SessionConfigOption`, category `Model`) — the
  current model-selection mechanism — are stable.
- `SessionUpdate::UsageUpdate` (context-window / cost reporting) is stable.
- `SessionUpdate::SessionInfoUpdate` (session title/metadata) is stable.
- Session listing (`ListSessionsRequest`/`ListSessionsResponse`) is stable.

The `unstable` flags only change the *Rust types you see* (e.g.
`unstable_boolean_config` turns `SetSessionConfigOptionRequest::value` into a
tagged enum). They do not change the wire format for the features above, so a
stable-surface agent stays compatible with an `unstable` client like Zed.

```toml
[dependencies]
# Stable surface (recommended for code-assistant):
agent-client-protocol = "0.14"
```

## Roles, not traits

There is no `Agent`/`Client` trait to implement anymore. Instead you pick a
**role** (`Agent`, `Client`, `Proxy`, `Conductor`) and register per-message
handlers on a connection builder:

```rust
use agent_client_protocol::{Agent, Client, ConnectionTo, Dispatch, Result, Stdio};
use agent_client_protocol::schema::{AgentCapabilities, InitializeRequest, InitializeResponse};

#[tokio::main]
async fn main() -> Result<()> {
    Agent
        .builder()
        .name("my-agent") // for debugging/tracing
        .on_receive_request(
            async move |req: InitializeRequest, responder, _cx: ConnectionTo<Client>| {
                responder.respond(
                    InitializeResponse::new(req.protocol_version)
                        .agent_capabilities(AgentCapabilities::new()),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        // ... one `on_receive_request` per request type, plus
        // `on_receive_notification` for notifications such as `session/cancel`.
        .on_receive_dispatch(
            async move |message: Dispatch, cx: ConnectionTo<Client>| {
                message.respond_with_error(
                    agent_client_protocol::util::internal_error("unhandled message"),
                    cx,
                )
            },
            agent_client_protocol::on_receive_dispatch!(),
        )
        .connect_to(Stdio::new())
        .await
}
```

Each request handler is an `AsyncFnMut(Req, Responder<Req::Response>, ConnectionTo<Client>)`.
You answer with `responder.respond(resp)` / `responder.respond_with_result(result)`
/ `responder.respond_with_error(err)`.

### Schema type paths

All wire types live under `agent_client_protocol::schema` (re-exported flatly in
`0.14`, e.g. `agent_client_protocol::schema::PromptRequest`). The role,
connection, builder and `Responder` types live at the crate root
(`agent_client_protocol::{Agent, Client, ConnectionTo, Responder, Stdio, ...}`).

## The dispatch loop, `spawn`, and long-running work

`on_*` handlers run **inside the connection's dispatch loop** and block it until
they return. This matters for the `session/prompt` handler: if it blocks while
the agent runs, the loop cannot process the `session/cancel` notification.

The pattern is to move the `Responder` into a spawned task and return
immediately:

```rust
.on_receive_request(
    async move |req: PromptRequest, responder, cx: ConnectionTo<Client>| {
        tokio::spawn(async move {
            let result = run_turn(cx, req).await; // long-running
            let _ = responder.respond_with_result(result);
        });
        Ok(()) // returns instantly; loop stays free for `session/cancel`
    },
    agent_client_protocol::on_receive_request!(),
)
```

| Pattern               | Blocks loop? | Use when                              |
|-----------------------|--------------|---------------------------------------|
| `on_*` callback       | yes          | quick decisions, need ordering        |
| `on_receiving_result` | yes          | process a reply before the next msg   |
| `block_task().await`  | no           | in a spawned task, need the reply     |
| `cx.spawn(..)` / `tokio::spawn` | no | long-running work, no ordering need   |

## Calling the client (filesystem, terminal, permission)

The connection is `Send + Clone`, so from a spawned task you call the client
directly — no `LocalSet`/`spawn_local` worker is needed:

```rust
let response = cx
    .send_request(acp::schema::ReadTextFileRequest::new(session_id, path))
    .block_task()
    .await?;

cx.send_request(acp::schema::CreateTerminalRequest::new(session_id, cmd))
    .block_task()
    .await?;

let outcome = cx
    .send_request(acp::schema::RequestPermissionRequest::new(session_id, tool_call, options))
    .block_task()
    .await?;
```

## Sending notifications

Session updates are fire-and-forget notifications:

```rust
cx.send_notification(acp::schema::SessionNotification::new(
    session_id.clone(),
    acp::schema::SessionUpdate::AgentMessageChunk(acp::schema::ContentChunk::new(
        acp::schema::ContentBlock::Text(acp::schema::TextContent::new("Thinking...")),
    )),
))?;
```

Useful `SessionUpdate` variants:

- `AgentMessageChunk` / `AgentThoughtChunk` / `UserMessageChunk` — streamed text
- `ToolCall` / `ToolCallUpdate` — tool-call lifecycle
- `Plan` — agent plan
- `SessionInfoUpdate` — **session title** and metadata
  (`SessionInfoUpdate::new().title("…")`)
- `UsageUpdate` — **context-window occupancy** and cost
  (`UsageUpdate::new(used_tokens, max_tokens).cost(Cost::new(amount, "USD"))`)
- `ConfigOptionUpdate` — updated session config options

## Model selection via session config options

Model selection is no longer a dedicated API (`SessionModelState`/
`set_session_model` were removed). It is now expressed as a generic **session
config option** with `category = Model`:

```rust
let option = acp::schema::SessionConfigOption::select(
    "model",            // config id (echoed back in session/set_config_option)
    "Model",            // label
    current_model_id,   // current value id
    groups,             // Vec<SessionConfigSelectGroup> or Vec<SessionConfigSelectOption>
)
.category(acp::schema::SessionConfigOptionCategory::Model);

// returned from new_session / load_session:
acp::schema::NewSessionResponse::new(session_id).config_options(vec![option]);
```

The client then sends `session/set_config_option`
(`SetSessionConfigOptionRequest { session_id, config_id, value }`); the agent
applies the change and replies with the refreshed `config_options`.

## Running over stdio

`Agent.builder()....connect_to(Stdio::new()).await` runs a server that only
responds to incoming messages. If you also need to initiate work for the
lifetime of the connection (e.g. drain a notification queue), use
`connect_with` and receive a `ConnectionTo<Client>` in your closure:

```rust
builder
    .connect_with(Stdio::new(), async move |conn| {
        while let Some(notification) = queue.recv().await {
            conn.send_notification(notification)?;
        }
        Ok::<(), acp::Error>(())
    })
    .await
```

`connect_with` runs the dispatch loop and your closure concurrently and returns
when either finishes (e.g. stdin reaches EOF).

## Error Handling

```rust
use agent_client_protocol::Error;

let error = Error::internal_error();
let error = Error::invalid_params();
let error = Error::method_not_found();

// Attach detail:
let error = Error::invalid_params().data("missing model id");
```

## Documentation

Full API documentation is available on [docs.rs](https://docs.rs/agent-client-protocol/latest/agent_client_protocol/).

## Resources

- **Crate**: https://crates.io/crates/agent-client-protocol
- **Documentation**: https://docs.rs/agent-client-protocol
- **Examples**: https://github.com/agentclientprotocol/rust-sdk/tree/main/src/agent-client-protocol/examples
  (see `simple_agent.rs`, `yolo_one_shot_client.rs`)
- **GitHub / Rust SDK**: https://github.com/agentclientprotocol/rust-sdk
