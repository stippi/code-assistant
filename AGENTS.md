# Repository Guidance

This file provides guidance to AI agents when working with code in this repository.
Additional documentation is available in the `docs` folder if needed.

## Essential Commands

### Building and Development
- `cargo check` - Test if the project compiles
- `cargo check --tests` - Test if the tests compile
- `cargo test` - Run all tests
- `cargo fmt --all -- --check` - Check code formatting
- `cargo clippy --all-targets --all-features -- -D warnings` - Run linter

### Testing Specific Components
- `cargo test --package code-assistant` - Test wiring binary
- `cargo test --package code-assistant-core` - Test domain layer
- `cargo test --package agent-core` - Test agent core
- `cargo test --package tools-core` - Test tool framework
- `cargo test --package llm` - Test LLM integration
- `cargo test --package web` - Test web functionality

## Architecture Overview

This is a Rust-based tool for AI-assisted code tasks with multiple operational modes.

### Crate Layers

```
Layer 0 (generic):    llm  command_executor  fs_explorer  sandbox  web  git  terminal  terminal_output
Layer 1 (generic):    tools_core        — tool trait, registry, render, spec, permissions
Layer 2 (generic):    agent_core        — agent loop, hook traits, dialect trait, AgentUi trait
Layer 3 (domain):     code_assistant_core — sessions, SessionService, event stream, UiEvent,
                                           tool impls, dialects (xml/caret), plugins, sub-agents
Layer 4 (frontends):  ui_gpui  ui_terminal  ui_acp  mcp_server
Layer 5 (binary):     code_assistant    — CLI, config, feature-gated frontend wiring
```

The binary feature-gates `gpui-frontend`, `terminal-frontend`, `acp-frontend`,
and `mcp-server` (all default). A `--no-default-features` build produces a
headless binary without gpui.

### Key Entry Points
- **Agent loop**: `crates/agent_core/src/runtime.rs`
- **Domain agent wrapper**: `crates/code_assistant_core/src/agent/runner.rs`
- **Tool trait & registry**: `crates/tools_core/src/`
- **Tool implementations**: `crates/code_assistant_core/src/tools/`
- **Tool dialects (xml/caret)**: `crates/code_assistant_core/src/tool_dialects/`
- **Plugins/hooks**: `crates/code_assistant_core/src/plugins/`
- **Session management**: `crates/code_assistant_core/src/session/`
- **GPUI frontend**: `crates/ui_gpui/src/`
- **Terminal frontend**: `crates/ui_terminal/src/`
- **MCP server**: `crates/mcp_server/src/`
- **ACP frontend**: `crates/ui_acp/src/`

### Tool Architecture
- **Core framework** (`tools_core`): `DynTool` trait, `ToolRegistry` (instance, not singleton), `ToolSpec` with capability tags
- **Tool implementations** live in `code_assistant_core::tools`
- **Tool modes** (configured per agent instance via `ToolDialect`):
  - `native` — LLM provider's native tool calling (default in `agent_core`)
  - `xml` — XML-based tool syntax in system messages
  - `caret` — triple-caret-fenced tool syntax in system messages

### LLM Integration (`crates/llm/`)
- Multi-provider support: Anthropic, OpenAI, Google Vertex, Ollama, OpenRouter, AI Core
- Recording/playback system for debugging and testing
- Configurable context windows and model selection

## Configuration

### MCP Server Mode
- Integrates with Claude Desktop as MCP server

### Agent Mode
- Supports terminal, Agent Client Protocol, and GPUI interfaces
- State persistence for continuing sessions

## Development Notes

### Testing
- Unit tests distributed across modules; integration tests in `crates/code_assistant/src/tests/`
- Mock implementations in `code_assistant_core` behind the `test-utils` feature
- Use `tools::test_registry()` (exported under `test-utils`) for deterministic tool tests

### UI Development
- GPUI frontend based on Zed's gpui and gpui-component with custom components
- Streaming processors per dialect in `code_assistant_core::tool_dialects/{xml,caret}/stream.rs`
- Theme support

### Tool Development
- Implement `DynTool` / `Tool` traits from `tools_core`
- Register in a `ToolRegistry` instance via `register_default_tools()` in `code_assistant_core`
- Capability tags (e.g. `read_only`, `edits_files`) replace the old `ToolScope` enum

## UI Communication Architecture

Two directions across one seam (`code_assistant_core::session`):

1. **UI → core: `SessionService`** (`session/service.rs`) — every command a
   frontend issues (create/load/delete session, send/queue message, switch
   model/sandbox/worktree, branching, skills, `request_stop`) is a typed async
   method returning `Result<T>`. Internally an actor: methods enqueue a
   closure on a command channel and await a oneshot reply; a single worker
   (spawned on the backend tokio runtime by the wiring) executes commands in
   order. `load_session` returns an owned `SessionSnapshot` (transcript incl.
   in-flight partial response, tool results, plan, activity, model/sandbox
   state); `SessionSnapshot::connect_events()` renders it as the canonical
   event sequence.

2. **Core → UI: broadcast `EventStream`** (`session/event_stream.rs`) — all
   notifications (streaming `DisplayFragment`s, `UiEvent`s) are published
   session-tagged; frontends `subscribe()` and filter by the session they
   view (sidebar-relevant events like activity/metadata pass regardless).
   A lagged subscriber gets `StreamError::Lagged` and resyncs via a fresh
   snapshot. The core does not know which session is "connected" or how many
   views exist.

### Concurrent Agent System
- **Multiple agents** can run concurrently, one per session; any number of
  frontends/views can observe them via the stream
- **`SessionEventPublisher`** (`session/instance.rs`) implements the
  `UserInterface` trait for the agent seam: it publishes everything and
  records per-session in-flight state (fragments of the streaming response,
  live tool statuses) that snapshots include; activity-state transition rules
  live in `SessionActivity`
- **Cancellation** is a core-side per-session flag (`request_stop`), checked
  by the agent at streaming checkpoints — works for background sessions too

### Frontend patterns
- **GPUI**: commands in `ui_gpui/src/app/commands.rs` (dispatched on the
  background executor), stream ingestion in `app/event_bridge.rs`
- **Terminal**: commands via the `Actions` struct, bridge task in
  `ui_terminal/src/app.rs`
- **ACP**: routes stream events to per-prompt `ACPUserUI` instances via its
  `active_uis` registry (`ui_acp/src/app.rs`); its session/prompt commands
  intentionally use `SessionManager` directly — the protocol-adapter needs
  (client-specified session ids, per-prompt agent starts, completion waiting)
  don't map onto `SessionService`
- The filesystem `SessionWatcher` still pushes `UiEvent`s directly into
  frontend channels (not via the stream) — a known remaining seam

(Below instructions copied from Zed's `.rules` file)

## GPUI

GPUI is a UI framework which also provides primitives for state and concurrency management.

### Context

Context types allow interaction with global state, windows, entities, and system services. They are typically passed to functions as the argument named `cx`. When a function takes callbacks they come after the `cx` parameter.

* `App` is the root context type, providing access to global state and read and update of entities.
* `Context<T>` is provided when updating an `Entity<T>`. This context dereferences into `App`, so functions which take `&App` can also take `&Context<T>`.
* `AsyncApp` and `AsyncWindowContext` are provided by `cx.spawn` and `cx.spawn_in`. These can be held across await points.

### `Window`

`Window` provides access to the state of an application window. It is passed to functions as an argument named `window` and comes before `cx` when present. It is used for managing focus, dispatching actions, directly drawing, getting user input state, etc.

### Entities

An `Entity<T>` is a handle to state of type `T`. With `thing: Entity<T>`:

* `thing.entity_id()` returns `EntityId`
* `thing.downgrade()` returns `WeakEntity<T>`
* `thing.read(cx: &App)` returns `&T`.
* `thing.read_with(cx, |thing: &T, cx: &App| ...)` returns the closure's return value.
* `thing.update(cx, |thing: &mut T, cx: &mut Context<T>| ...)` allows the closure to mutate the state, and provides a `Context<T>` for interacting with the entity. It returns the closure's return value.
* `thing.update_in(cx, |thing: &mut T, window: &mut Window, cx: &mut Context<T>| ...)` takes a `AsyncWindowContext` or `VisualTestContext`. It's the same as `update` while also providing the `Window`.

Within the closures, the inner `cx` provided to the closure must be used instead of the outer `cx` to avoid issues with multiple borrows.

Trying to update an entity while it's already being updated must be avoided as this will cause a panic.

When  `read_with`, `update`, or `update_in` are used with an async context, the closure's return value is wrapped in an `anyhow::Result`.

`WeakEntity<T>` is a weak handle. It has `read_with`, `update`, and `update_in` methods that work the same, but always return an `anyhow::Result` so that they can fail if the entity no longer exists. This can be useful to avoid memory leaks - if entities have mutually recursive handles to each other they will never be dropped.

### Concurrency

All use of entities and UI rendering occurs on a single foreground thread.

`cx.spawn(async move |cx| ...)` runs an async closure on the foreground thread. Within the closure, `cx` is an async context like `AsyncApp` or `AsyncWindowContext`.

When the outer cx is a `Context<T>`, the use of `spawn` instead looks like `cx.spawn(async move |handle, cx| ...)`, where `handle: WeakEntity<T>`.

To do work on other threads, `cx.background_spawn(async move { ... })` is used. Often this background task is awaited on by a foreground task which uses the results to update state.

Both `cx.spawn` and `cx.background_spawn` return a `Task<R>`, which is a future that can be awaited upon. If this task is dropped, then its work is cancelled. To prevent this one of the following must be done:

* Awaiting the task in some other async context.
* Detaching the task via `task.detach()` or `task.detach_and_log_err(cx)`, allowing it to run indefinitely.
* Storing the task in a field, if the work should be halted when the struct is dropped.

A task which doesn't do anything but provide a value can be created with `Task::ready(value)`.

### Elements

The `Render` trait is used to render some state into an element tree that is laid out using flexbox layout. An `Entity<T>` where `T` implements `Render` is sometimes called a "view".

Example:

```
struct TextWithBorder(SharedString);

impl Render for TextWithBorder {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().border_1().child(self.0.clone())
    }
}
```

Since `impl IntoElement for SharedString` exists, it can be used as an argument to `child`. `SharedString` is used to avoid copying strings, and is either an `&'static str` or `Arc<str>`.

UI components that are constructed just to be turned into elements can instead implement the `RenderOnce` trait, which is similar to `Render`, but its `render` method takes ownership of `self`. Types that implement this trait can use `#[derive(IntoElement)]` to use them directly as children.

The style methods on elements are similar to those used by Tailwind CSS.

If some attributes or children of an element tree are conditional, `.when(condition, |this| ...)` can be used to run the closure only when `condition` is true. Similarly, `.when_some(option, |this, value| ...)` runs the closure when the `Option` has a value.

### Input events

Input event handlers can be registered on an element via methods like `.on_click(|event, window, cx: &mut App| ...)`.

Often event handlers will want to update the entity that's in the current `Context<T>`. The `cx.listener` method provides this - its use looks like `.on_click(cx.listener(|this: &mut T, event, window, cx: &mut Context<T>| ...)`.

### Actions

Actions are dispatched via user keyboard interaction or in code via `window.dispatch_action(SomeAction.boxed_clone(), cx)` or `focus_handle.dispatch_action(&SomeAction, window, cx)`.

Actions with no data defined with the `actions!(some_namespace, [SomeAction, AnotherAction])` macro call. Otherwise the `Action` derive macro is used. Doc comments on actions are displayed to the user.

Action handlers can be registered on an element via the event handler `.on_action(|action, window, cx| ...)`. Like other event handlers, this is often used with `cx.listener`.

### Notify

When a view's state has changed in a way that may affect its rendering, it should call `cx.notify()`. This will cause the view to be rerendered. It will also cause any observe callbacks registered for the entity with `cx.observe` to be called.

### Entity events

While updating an entity (`cx: Context<T>`), it can emit an event using `cx.emit(event)`. Entities register which events they can emit by declaring `impl EventEmittor<EventType> for EntityType {}`.

Other entities can then register a callback to handle these events by doing `cx.subscribe(other_entity, |this, other_entity, event, cx| ...)`. This will return a `Subscription` which deregisters the callback when dropped.  Typically `cx.subscribe` happens when creating a new entity and the subscriptions are stored in a `_subscriptions: Vec<Subscription>` field.

### Recent API changes

GPUI has had some changes to its APIs. Always write code using the new APIs:

* `spawn` methods now take async closures (`AsyncFn`), and so should be called like `cx.spawn(async move |cx| ...)`.
* Use `Entity<T>`. This replaces `Model<T>` and `View<T>` which no longer exist and should NEVER be used.
* Use `App` references. This replaces `AppContext` which no longer exists and should NEVER be used.
* Use `Context<T>` references. This replaces `ModelContext<T>` which no longer exists and should NEVER be used.
* `Window` is now passed around explicitly. The new interface adds a `Window` reference parameter to some methods, and adds some new "*_in" methods for plumbing `Window`. The old types `WindowContext` and `ViewContext<T>` should NEVER be used.
