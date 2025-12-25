# Repository Guidance

This file provides guidance to AI agents when working with code in this repository.

## Essential Commands

### Building and Development
- `cargo build` - Build the project
- `cargo build --release` - Build optimized release version
- `cargo test` - Run all tests
- `cargo fmt --all -- --check` - Check code formatting
- `cargo clippy --all-targets --all-features -- -D warnings` - Run linter (currently disabled in CI)

### Running the Application
- `cargo run -- --task "description"` - Run in agent mode with a task
- `cargo run -- --ui` - Start with GUI interface
- `cargo run -- --ui --task "description"` - Start GUI with initial task
- `cargo run -- server` - Run as MCP server
- `cargo run -- --help` - Show all available options

### Testing Specific Components
- `cargo test --package code-assistant` - Test main crate
- `cargo test --package llm` - Test LLM integration
- `cargo test --package web` - Test web functionality

## Architecture Overview

This is a Rust-based CLI tool for AI-assisted code tasks with multiple operational modes:

### Core Structure
- **Workspace Layout**: Multi-crate workspace with 3 main crates:
  - `crates/code_assistant/` - Main application logic
  - `crates/llm/` - LLM provider integrations (Anthropic, OpenAI, Vertex, Ollama, etc.)
  - `crates/web/` - Web-related functionality (Perplexity, web fetches)

### Key Components
- **Agent System** (`src/agent/`): Core AI agent logic with persistence and tool execution
- **Tool System** (`src/tools/`): Extensible tool framework with implementations for file operations, command execution, and web searches
- **UI Framework** (`src/ui/`): Dual interface support:
  - Terminal UI with rustyline
  - GUI using Zed's GPUI framework
- **Session Management** (`src/session/`): Multi-session support with persistence
- **MCP Server** (`src/mcp/`): Model Context Protocol server implementation

### Tool Architecture
- **Core Framework** (`src/tools/core/`): Dynamic tool registry and execution system
- **Tool Implementations** (`src/tools/impls/`): File operations, command execution, search, web fetch
- **Tool Modes**:
  - `native` - Uses LLM provider's native tool calling
  - `xml` - Custom XML-based tool syntax in system messages

### LLM Integration
- Multi-provider support: Anthropic, OpenAI, Google Vertex, Ollama, OpenRouter, AI Core
- Recording/playback system for debugging and testing
- Configurable context windows and model selection

## Configuration

### MCP Server Mode
- Requires `~/.config/code-assistant/projects.json` for project definitions
- Integrates with Claude Desktop as MCP server
- Environment variables: `PERPLEXITY_API_KEY`, `ANTHROPIC_API_KEY`, etc.

### Agent Mode
- Supports both terminal and GUI interfaces
- State persistence for continuing sessions
- Working memory system for codebase exploration

## Development Notes

### Testing
- Unit tests distributed across modules
- Integration tests in `src/tests/`
- Mock implementations for testing (`src/tests/mocks.rs`)

### UI Development
- GPUI-based GUI with custom components
- Streaming JSON/XML processors for real-time updates
- Theme support and file type icons

### Tool Development
- Implement `DynTool` trait for new tools
- Register in tool registry
- Support both sync and async operations
- Follow existing patterns in `src/tools/impls/`

## UI Communication Architecture

### Communication Channels
There are **two main communication patterns** between components and the UI:

1. **Direct UserInterface trait calls** (primary pattern):
   - Agent calls methods like `begin_llm_request()`, `display()`, `update_tool_status()`, etc.
   - The `display()` method takes a `UIMessage` which can wrap a `UiEvent`
   - Can send any UI event by wrapping it: `UIMessage::UiEvent(event)`
   - Events go into the main UI event queue processed by the first task

2. **Backend thread communication** (session management):
   - Used for session management operations (create, delete, list sessions)
   - Has separate `BackendEvent`/`BackendResponse` types and channels
   - Handled by a second task running concurrently
   - Operations: `LoadSession`, `CreateNewSession`, `ListSessions`, etc.

### Event Queue Architecture
- **Two event queues** running concurrently with separate tasks
- **Task 1**: Processes `UiEvent`s from UserInterface trait calls
- **Task 2**: Handles session management `BackendEvent`s and `BackendResponse`s
- Architecture acknowledged as "messy" and should be cleaned up eventually

### Concurrent Agent System
- **Multiple agents** can run concurrently, one per session
- **Only one agent** is connected to the UI at any time
- **ProxyUI system**: Each session gets a `ProxyUI` instance that only forwards events and method calls to the real UI when that session is "connected"
- **Session states**:
  - **Connected**: Session is actively connected to UI (user clicked on chat item in sidebar)
  - **Active**: Agent loop is currently running in the session (can be active without being connected)
- **Session switching**: User clicks chat items in sidebar to connect/activate different sessions

### Key Implementation Details
- Agent-to-UI communication should use existing `self.ui.display(UIMessage::UiEvent(...))` pattern
- Avoid overcomplicating with new channels or architectures
- Leverage the ProxyUI system for proper session isolation
- Session metadata updates can be sent directly via the existing event system

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
