# Repository Guidance

This file provides guidance to AI agents when working with code in this repository.
Additional documentation is available in the `docs` folder if needed.

## Essential Commands

- `cargo check` - Test if the project compiles
- `cargo check --tests` - Test if the tests compile
- `cargo test` - Run all tests
- `cargo test --package <crate>` - Test a single crate (names in Crate Layers below)
- `cargo fmt --all -- --check` - Check code formatting
- `cargo clippy --all-targets --all-features -- -D warnings` - Run linter

## Architecture Overview

This is a Rust-based AI coding agent harness with multiple operational modes.

### Crate Layers

```
Layer 0 (generic):    llm  command_executor  pty_session  fs_explorer  sandbox  web  git  terminal  terminal_output
Layer 1 (generic):    tools_core        — tool trait, registry, render, spec, permissions
                      mcp_client        — wraps MCP server tools as registry tools
Layer 2 (generic):    agent_core        — agent loop, hook traits, dialect trait, AgentUi trait
Layer 3 (domain):     code_assistant_core — sessions, SessionService, event stream, UiEvent,
                                           tool impls, dialects (xml/caret), plugins, sub-agents
Layer 4 (frontends):  ui_gpui  ui_terminal  ui_acp  mcp_server
Layer 5 (binary):     code_assistant    — CLI, config, feature-gated frontend wiring
```

### Key Entry Points
- **Agent loop**: `crates/agent_core/src/runtime.rs`
- **Domain agent wrapper**: `crates/code_assistant_core/src/agent/runner.rs`
- **Tool implementations**: `crates/code_assistant_core/src/tools/`
- **Tool dialects (xml/caret)**: `crates/code_assistant_core/src/tool_dialects/`
- **Plugins/hooks**: `crates/code_assistant_core/src/plugins/`
- **Session management**: `crates/code_assistant_core/src/session/`
- **MCP client config/registration binding**: `crates/code_assistant_core/src/tools/mcp.rs`

### Tool Architecture
- `ToolRegistry` is an instance, not a singleton; `ToolSpec` carries capability tags (e.g. `read_only`)
- **Tool modes** (configured per agent instance via `ToolDialect`):
  - `native` — LLM provider's native tool calling (default in `agent_core`)
  - `xml` — XML-based tool syntax in system messages
  - `caret` — triple-caret-fenced tool syntax in system messages

### LLM Integration (`crates/llm/`)
- Multi-provider support: Anthropic, OpenAI, Google Vertex, Ollama, OpenRouter, AI Core
- Recording/playback system for debugging and testing
- Configurable context windows and model selection

## Configuration

### MCP Client Mode
- Connects to MCP servers configured in `<config_dir>/mcp-servers.json`
  (stdio or HTTP, via the `rmcp` SDK) and registers their tools in the
  `ToolRegistry` as `mcp__<server>__<tool>`; projects can add servers in a
  `.mcp.json` at the project root
- The registry is rebuilt from the current config at the start of every
  agent run (`ToolRegistryProvider` seam)
- Details: `docs/mcp-client-mode.md`, `docs/project-scoped-mcp-servers.md`

### Permission Tiers
- Per-session setting deciding when the agent asks before running a tool:
  `bypass-all` (default), `write-tools` (ask for anything not tagged
  `read_only`), `all-tools` (ask always)
- Gate lives in the agent loop (`tools_core::ToolPermissions`); prompts go
  through the `PermissionMediator` seam; see `docs/permission-tiers.md`

## Development Notes

### Testing
- Unit tests distributed across modules; integration tests in `crates/code_assistant/src/tests/`
- Mock implementations in `code_assistant_core` behind the `test-utils` feature
- Use `tools::test_registry()` (exported under `test-utils`) for deterministic tool tests

### UI Development
- GPUI frontend based on Zed's gpui and gpui-component with custom components
- GPUI API reference (contexts, entities, tasks, elements, actions, events):
  `docs/gpui-reference.md`

## UI Communication Architecture

Two directions across one seam (`code_assistant_core::session`):

1. **UI → core: `SessionService`** (`session/service.rs`) — every frontend
   command is a typed async method. Internally an actor: a single worker on
   the backend tokio runtime executes commands in order. `load_session`
   returns an owned `SessionSnapshot`; `connect_events()` renders it as the
   canonical event sequence.

2. **Core → UI: broadcast `EventStream`** (`session/event_stream.rs`) — all
   notifications are published session-tagged; frontends `subscribe()` and
   filter by the session they view. A lagged subscriber gets
   `StreamError::Lagged` and resyncs via a fresh snapshot. The core does not
   know which session is "connected" or how many views exist.

Consequences:
- Multiple agents run concurrently, one per session; `SessionEventPublisher`
  (`session/instance.rs`) implements the agent's `UserInterface` and records
  the in-flight state that snapshots include
- Cancellation is a core-side per-session flag (`request_stop`), checked by
  the agent at streaming checkpoints
- ACP's session/prompt commands intentionally bypass `SessionService` and
  use `SessionManager` directly (protocol-adapter needs don't map onto it)
- The filesystem `SessionWatcher` still pushes `UiEvent`s directly into
  frontend channels, not via the stream — a known remaining seam

## GPUI API rules

GPUI has had some changes to its APIs. Always write code using the new APIs:

* `spawn` methods now take async closures (`AsyncFn`), and so should be called like `cx.spawn(async move |cx| ...)`.
* Use `Entity<T>`. This replaces `Model<T>` and `View<T>` which no longer exist and should NEVER be used.
* Use `App` references. This replaces `AppContext` which no longer exists and should NEVER be used.
* Use `Context<T>` references. This replaces `ModelContext<T>` which no longer exists and should NEVER be used.
* `Window` is now passed around explicitly. The new interface adds a `Window` reference parameter to some methods, and adds some new "*_in" methods for plumbing `Window`. The old types `WindowContext` and `ViewContext<T>` should NEVER be used.
