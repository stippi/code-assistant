# Agent Core Extraction — Analysis & Vision

> Status: analysis, no code. The goal is twofold: (a) a reusable agent core (comparable
> to the Claude Code Agent SDK) that `code-assistant` uses as one of several consumers,
> and (b) breaking up the monolithic `code_assistant` crate into independent, layered
> crates — including the UIs — so that the `code_assistant` crate itself shrinks to a
> thin wiring binary.

## 1. Vision

Extract a generic agent core from today's `code_assistant` crate into a standalone
crate (or a small family of crates). Other applications can embed this core and, through
clearly defined extension points:

- register **their own tools**,
- bring in **their own tool invocation formats** (native, XML, Caret, custom) as plugins,
- hook **their own behavior plugins** into fixed points of the agent loop
  (system prompt, pre-/post-LLM, pre-/post-tool, special tools, compaction, …),
- attach **their own UI / persistence / permission adapters**,

all without modifying the core. At the same time, the breakup continues *above* the
generic core: sessions, persistence, the concrete tools, the text dialects, and the
plugins move into a domain crate; the GPUI and terminal frontends become crates of their
own. The target picture as a layered crate graph:

```
Layer 0 (exists, generic):   llm  command_executor  fs_explorer  sandbox  web  git  terminal
Layer 1 (new, generic):      tools_core             <-- tool trait, registry, render, spec
Layer 2 (new, generic):      agent_core             <-- agent loop, traits, hooks, dialect trait
Layer 3 (new, domain):       code_assistant_core    <-- sessions, persistence, UiEvent,
                                                        tool impls, dialects, plugins, sub-agents
Layer 4 (new, frontends):    ui_gpui  ui_terminal  acp  mcp
Layer 5 (exists, thin):      code_assistant         <-- CLI, config, wiring
```

Other applications bind only to `agent_core` + `tools_core` (Layers 1–2). Layers 3–4
are code-assistant product code, but split so that each frontend is an independent
compile unit (touching agent logic no longer recompiles anything gpui-related, and the
binary can feature-gate entire frontends).

### 1.1 The domain layer — the UIs are not generic, and that's fine

The GPUI and terminal UIs are not generic agent UIs: they render sessions, branching,
worktrees, plans, sandbox state. They can never sit on `agent_core` alone, and forcing
them to (via `AgentUiEvent::Custom(Box<dyn Any>)` downcasts everywhere) would produce
exactly the cumbersome code this refactoring must avoid. The resolution is the
**domain crate `code_assistant_core`**: it owns the full `UiEvent` vocabulary, sessions,
persistence types, the concrete tools, and the XML/Caret dialects. The frontends are
frontends *of that domain*, not of the generic core.

The domain `UiEvent` **embeds** the core events
(`enum UiEvent { Agent(AgentUiEvent), Session(...), Worktree(...), ... }`) so the
frontends consume one concrete, fully-typed enum. The `Custom(Box<dyn Any>)` escape
hatch in the core exists only for third-party consumers — zero downcasts in our own
code.

By line count, the UI *is* the actual god-module: of ~77k lines in `code_assistant`,
`ui/` holds ~43k (gpui ~22.6k, terminal ~11.2k, streaming ~7k); agent + tools + session
together are ~22k. Today `ui/` references `crate::persistence` 37× and `crate::session`
16×, while `agent/` and `tools/` reach back into `ui/` (the `UserInterface` trait and
`UiEvent`). The breakup dissolves that cycle by construction: the trait moves down into
`agent_core`, the app-specific events into `code_assistant_core`, and the
implementations up into the frontend crates.

### 1.2 Breakup ground rules (how to avoid "more and cumbersome code")

1. **No shared dumping-ground crate.** When a stubborn cycle appears, the lazy fix is a
   `common`/`types` crate everything depends on — it grows forever. Instead, every type
   lives in the lowest crate that *owns the concept*, and cycles get broken with the
   patterns already in use (events, channels, small traits).
2. **Watch the generics budget.** Generic parameters that infect the domain and UI
   crates are the single biggest "cumbersome" risk — see §7.9; decide in favor of
   fewer generics before the crate split.
3. **No over-fragmentation.** Every crate boundary is a public-API commitment and
   friction while iterating. The graph above adds ~5 crates beyond the generic core —
   don't go finer initially. `acp`/`mcp` may start as modules in `code_assistant_core`
   or the binary and be carved out when something forces it; `code_assistant_core`
   itself may split later (tools vs. session) if a reason appears.
4. **Wiring-crate scope.** "`code_assistant` just wires everything up" is realistic
   (today's `app/` module is ~500 lines), but session *management* (manager, instances,
   watcher — ~3.4k lines) belongs in `code_assistant_core`, not the binary. The binary
   keeps: CLI parsing, config loading, builder calls, frontend selection.

> **Reading note:** the sections below predate the full-breakup decision and often name
> `code_assistant` as the post-extraction home of dialects, plugins, sessions, etc.
> With the layering above, that home is the domain crate `code_assistant_core`; the
> `code_assistant` binary keeps only wiring. The analysis itself is unaffected.

> **Important: text-based tool invocation formats (XML / Caret) do NOT belong in the core.**
> Tools themselves are already syntax-agnostic today and shall stay that way. What depends
> on a specific format is solely the translation between LLM response/stream and abstract
> `ToolRequest`s, plus the presentation of tools in the system prompt. The core defines a
> minimal trait for this (see §3.7) **and ships exactly one default implementation:
> native tool calling via the LLM API.** "Native" is not a text format but the API
> mechanism itself — a minimal consumer therefore never has to deal with the syntax topic
> at all (see §3.11). The concrete XML/Caret implementations remain part of
> `code_assistant`.

---

## 2. Status quo: where does the coupling live today?

The following list marks the concrete places in `code-assistant` that currently make
application-specific assumptions about the agent. This is the list of points that the
refactoring must move either "up" (into the core) or "down" (into the consumer).

### 2.1 `Agent` runner (`crates/code_assistant/src/agent/runner.rs`)

`Agent::new` expects an `AgentComponents` bundle full of concrete types:

- `Box<dyn LLMProvider>` — generic, no problem.
- `Box<dyn ProjectManager>` — code-assistant-specific (multiple projects, file trees).
- `Box<dyn CommandExecutor>` — generic, no problem.
- `Arc<dyn UserInterface>` — the UI trait contains many code-assistant-specific events.
- `Box<dyn AgentStatePersistence>` — the trait exists, but `SessionState` is concrete.
- `Option<Arc<dyn PermissionMediator>>` — generic enough.
- `Option<Arc<dyn SubAgentRunner>>` — the sub-agent concept and its UI adapters are
  code-assistant-specific.

Beyond that, the agent itself holds stateful code-assistant concepts:

- `plan: PlanState` — the plan is functionality of the `update_plan` tool, not a
  generic agent mechanism.
- `tool_scope: ToolScope` with variants `Agent`, `AgentWithDiffBlocks`, `SubAgent…` —
  bundles several concepts (sub-agent, diff blocks).
- `message_nodes / active_path / next_node_id` — branching model based on
  `crate::persistence::MessageNode`.
- `tool_executions: Vec<ToolExecution>` — generic, but bound to `AnyOutput`, which in
  turn references the global `ToolRegistry` for (de-)serialization.
- `cached_system_prompts`, `model_hint`, `session_model_config`, `context_limit_override`
  — per-model system prompt selection, compaction threshold, etc.
- `file_trees`, `available_projects` — multi-project concept.
- `enable_naming_reminders`, `session_name`, `pending_message_ref` — session-level logic
  that does not need to be generic in the core.

In the loop implementation itself there is a whole series of **hard special-case paths**:

- `if tool_requests.iter().any(|r| r.name == "complete_task")` → loop breaks.
  **Caution: a `complete_task` tool does not exist** (neither in `tools/impls/` nor in
  `register_default_tools()`). In XML/Caret mode the parser rejects unknown tools, so the
  path is unreachable there. In native mode, however, extraction does *not* validate
  against the registry (`parse_tool_use_blocks` passes `ToolUse` blocks through
  unfiltered) — so the path fires if the model hallucinates `complete_task`, and then
  terminates the loop **silently with a dangling `ToolUse` without a ToolResult** in the
  history. Above all, though, the path is **load-bearing for the test infrastructure**:
  `MockLLMProvider::new` (`tests/mocks.rs`) automatically inserts a `complete_task`
  response as the loop terminator (see §6, test migration). The path can be removed, but
  only after the mocks have been switched to the natural loop ending ("no tool requests
  → GetUserInput").
- `if tool_request.name == "name_session"` → set title from the input, treat the tool as
  "hidden", update `tool_executions` directly, no UI update.
- `if tool_request.name == "update_plan" && success` → `save_plan_snapshot_to_last_…`.
- `can_run_in_parallel` → hardcoded to `spawn_agent` with `mode=read_only`.
- `execute_spawn_agent_parallel` → its own code path with concrete `SpawnAgentTool`,
  `SpawnAgentInput`, its own `ToolContext` build, its own `ToolStatus` handling.
- `inject_naming_reminder_if_needed` → appends a system reminder to the last user
  message, because the `session_name` concept lives inside the core.
- `is_prompt_too_long_error` and `replace_large_tool_results` → pattern matching on
  provider error texts plus a fallback mechanism that substitutes `PromptTooLongError`
  as tool output; the mechanism is generic, but the candidate selection ("only the last
  user-message turn set") is policy.
- `is_retryable_streaming_error` → heuristics on error strings. Generic.
- `should_trigger_compaction` / `perform_compaction` → hard `CONTEXT_USAGE_THRESHOLD`,
  hardcoded compaction prompt from `resources/compaction_prompt.md`.
- `read_guidance_files` (`AGENTS.md`, `CLAUDE.md`, `~/.config/code-assistant/AGENTS.md`)
  — code-assistant-specific behavior.
- System prompt construction: `generate_system_message(...)` + multi-project file trees +
  AGENTS.md guidance — a concrete prompt construction.
- **Format-on-save path** (overlooked in the first version of this analysis):
  `execute_tool` detects whether a tool modified its input during execution
  (`input_modified`), then sends `UpdateToolParameter` events and, via
  `update_message_history_with_formatted_tool` / `update_tool_call_in_text_static`,
  **rewrites the tool call inside the message-history text** — using
  `get_formatter(tool_syntax)` and the byte offsets
  `ToolRequest::start_offset/end_offset`. This is a hard dialect coupling in the middle
  of the loop: the core needs a dialect capability "format a ToolRequest back into text"
  for it (see §3.7). The offsets on `ToolRequest` exist precisely for this path.

### 2.2 `ToolContext` (`crates/code_assistant/src/tools/core/tool.rs`)

```rust
pub struct ToolContext<'a> {
    pub project_manager: &'a dyn crate::config::ProjectManager,
    pub command_executor: &'a dyn CommandExecutor,
    pub plan: Option<&'a mut PlanState>,
    pub ui: Option<&'a dyn crate::ui::UserInterface>,
    pub tool_id: Option<String>,
    pub permission_handler: Option<&'a dyn PermissionMediator>,
    pub sub_agent_runner: Option<&'a dyn crate::agent::SubAgentRunner>,
}
```

Today the tool context is a "Swiss army knife" carrying all the subsystems that
`code-assistant` needs. For a generic crate this is too concrete: tools from third-party
applications need neither `ProjectManager` nor `PlanState` nor `SubAgentRunner`.

### 2.3 `ToolRegistry` (`crates/code_assistant/src/tools/core/registry.rs`)

- `ToolRegistry::global()` is a process-wide singleton.
- `register_default_tools()` hardcodes registration of all 18 code-assistant tools.
- `is_tool_in_scope` filters based on the `ToolScope` enum, whose variants are
  application-specific.
- `ToolRegistry::register` consults **yet another singleton**: `ToolsConfig::global()`
  (`tools/core/config.rs`, loads `tools.json` containing e.g. the Perplexity API key)
  for `tool.is_available(config)`. When decoupling, the availability configuration must
  be injected at registration time.
- The registry is read **not only by the agent loop**: the stream processors
  (`ui/streaming/{xml,caret,json}_processor.rs`, each calling
  `ToolRegistry::global().is_tool_hidden(name, ToolScope::Agent)` — scope hardcoded!),
  `tools/core/title.rs` (title templates), and the MCP handler (see §2.12) all access
  the singleton. "Removing the singleton" therefore also touches the UI layer.

### 2.4 `ToolSpec` / `ToolScope` (`crates/code_assistant/src/tools/core/spec.rs`)

```rust
pub enum ToolScope {
    McpServer,
    Agent,
    AgentWithDiffBlocks,
    SubAgentReadOnly,
    SubAgentDefault,
}
```

This enumeration mixes several orthogonal concepts (MCP server mode, sub-agent mode,
diff-blocks variant) into a single enum. Generically, tags / capabilities would be better.

### 2.5 Tool filters (`crates/code_assistant/src/tools/tool_use_filter.rs`)

- `is_explore_tool`, `is_write_tool`, `SmartToolFilter::is_read_tool` contain hardcoded
  lists of tool names from `code-assistant`.
  (`is_write_tool` is currently `#[allow(dead_code)]`, i.e. has no callers.)
- The `ToolUseFilter` trait itself is clean — but the `SmartToolFilter` is
  **constructed hardcoded** at its usage sites (in `parser_registry.rs` during parsing
  and in the XML/Caret stream processors), not injected. A consumer therefore cannot
  swap the filter today without patching those sites.

### 2.6 Parser / syntax (`crates/code_assistant/src/tools/parser_registry.rs`,
`tools/parse.rs`, `tools/formatter.rs`, `tools/system_message.rs`)

- Tools themselves are already dialect-free today — very good. What depends on a
  concrete tool syntax is only the translation between LLM stream/response and abstract
  `ToolRequest`s, plus the presentation in the system prompt.
- The `ToolInvocationParser` trait is clean, but registration goes through the fixed
  enum `ToolSyntax { Native, Xml, Caret }`. A third-party application cannot plug in its
  own format without patching the core.
- The parser directly accesses `ToolRegistry::global()` in several places
  (schema-driven conversion).
- `is_multiline_param` contains a hardcoded allow-list of concrete parameter names
  (`content`, `command_line`, `diff`, `message`, `old_text`, `new_text`).
- The XML/Caret documentation generators invent example values based on parameter names
  (`project`, `path`, `regex`, `command_line`, `working_dir`, `url`, …) — again
  code-assistant vocabulary.
- `system_message::generate_system_message` loads embedded Markdown templates and the
  model mapping from `resources/`.

In the target picture, this whole file group disappears from the agent core and becomes
an internal module of `code_assistant` (see §3.7).

### 2.7 UI trait (`crates/code_assistant/src/ui/mod.rs`,
`crates/code_assistant/src/ui/ui_events.rs`)

The `UserInterface` trait itself is small, but `UiEvent` is huge and contains heavily
application-specific variants:

- `UpdateSessionMetadata`, `UpdateSessionActivityState`, `RefreshChatList`,
  `UpdateChatList`, `BranchSwitched`, `StartMessageEdit`, `MessageEditReady`,
  `UpdateBranchInfo`, `UpdateWorktreeData`, `UpdateSandboxPolicy`, `CancelSubAgent`,
  `PersistUiState`, `RefreshCurrentSession`, `AppendMessages`, `ResourceLoaded`,
  `ResourceWritten`, `DirectoryListed`, `ResourceDeleted`, `UpdatePlan`, …

The events that are genuinely relevant to the agent core are small in comparison:
`StreamingStarted/Stopped`, `RollbackStreaming`, `UpdateToolStatus`, `UpdateToolParameter`,
`AppendToTextBlock`, `AppendToThinkingBlock`, `StartTool`, `EndTool`, …

### 2.8 Persistence (`crates/code_assistant/src/persistence.rs`,
`crates/code_assistant/src/agent/persistence.rs`; `SessionState` itself lives in
`crates/code_assistant/src/session/mod.rs`)

- `SessionState` contains `message_nodes`, `active_path`, `tool_executions`, `plan`,
  `config: SessionConfig`, `next_request_id`, `model_config: SessionModelConfig`. The
  branch-tree structure is conceptually general; the `plan` and `model_config` fields
  are not.
- `AgentStatePersistence::save_agent_state(state: SessionState)` therefore couples the
  trait hard to the concrete structure.
- `SerializedToolExecution::deserialize` again accesses `ToolRegistry::global()`.

### 2.9 Sub-agents (`crates/code_assistant/src/agent/sub_agent.rs`)

- `SubAgentRunner` as a trait is *almost* okay — but the signature
  `run(parent_tool_id, instructions, tool_scope: ToolScope, require_file_references)`
  references the `ToolScope` enum. If `ToolScope` is replaced by capabilities (§3.6),
  this signature must be adapted along with it; in its current form the trait cannot
  move into the core.
- Beyond that, the default runner mixes a great many concrete aspects (sandbox,
  `DefaultProjectManager`, `SessionConfig`, its own UI adapters, the
  `SubAgentToolCall`/`SubAgentOutput` JSON shape for custom renderers).
- The `spawn_agent` tool output is tightly tied to the UI JSON shape.

### 2.10 Special tools with fixed meaning in the loop

The following tool names are *strings hardwired into the agent loop*:

| Tool             | Wired where                                   | Effect |
|------------------|-----------------------------------------------|--------|
| `complete_task`  | `manage_tool_execution`                        | breaks the loop — **tool does not exist; only reachable in native mode, and used by the test infrastructure as a loop terminator (see §2.1)** |
| `name_session`   | `execute_tool` (before the standard path)      | sets `session_name`, no UI update |
| `update_plan`    | after successful `execute_tool`                | stores a plan snapshot in the MessageNode |
| `spawn_agent`    | `can_run_in_parallel`, `execute_spawn_agent_parallel` | enables parallel execution & special UI (only with ≥2 read-only calls in the same turn; single calls run sequentially) |
| `parse_error`    | `agent::types`, `persistence`                  | pseudo-tool for parse errors |

Plus implicitly via the `SmartToolFilter`: `read_files`, `list_files`, `list_projects`,
`search_files`, `glob_files`, `web_fetch`, `web_search` (read), `write_file`,
`replace_in_file`, `delete_files` (write), `execute_command` (write).

### 2.11 Resources / templates

- `resources/compaction_prompt.md` — fixed compaction prompt
- `resources/tool_use_intro.md` — introduction of the system-prompt tool description
- `resources/system_prompts/{default, claude, codex}.md` + `mapping.json` —
  model-specific base prompts with `{{syntax}}` and `{{tools}}` placeholders.

### 2.12 MCP server as a second in-process consumer (`crates/code_assistant/src/mcp/handler.rs`)

The MCP handler is already a second consumer of the tool infrastructure today — and
thus a good reality check for the extraction:

- uses `ToolRegistry::global()` for `tools/list` and `tools/call`,
- filters via `ToolScope::McpServer`,
- constructs `ToolContext` directly (with `plan: None`, `ui: None`, …) and deliberately
  ignores `input_modified`.

Every change to `ToolScope` (→ capabilities), `ToolContext` (→ extensions), and the
registry (→ instance) must migrate the MCP handler along with it. This is worked into
the phase plan (§6).

---

## 3. Target architecture

### 3.1 Crate split

```
agent_core
├── lib.rs                  (re-exports)
├── agent/
│   ├── runtime.rs          (AgentRuntime, AgentLoop)
│   ├── config.rs           (AgentConfig — non-session)
│   ├── flow.rs             (LoopFlow, IterationOutcome)
│   └── error.rs
├── messages/
│   └── tree.rs             (MessageTree, NodeId — optional feature)
├── hooks/                  (see §3.5)
│   ├── prompt.rs
│   ├── lifecycle.rs
│   ├── tool_dispatch.rs
│   ├── compaction.rs
│   └── retry.rs
├── dialect/
│   ├── mod.rs              (ToolDialect trait, StreamProcessor trait)
│   └── native.rs           (default: native tool calling; today's json_processor
│                            as the stream processor — the only implementation in
│                            the core)
├── persistence.rs          (StatePersistence trait, AgentSnapshot)
├── ui.rs                   (AgentUi trait, AgentUiEvent — minimal set)
├── permissions.rs          (PermissionMediator trait)
└── test_utils/             (feature = "test-utils": ScriptedLLMProvider,
                             RecordingUi, InMemoryPersistence — so consumers can
                             test their plugins/hooks without building their own
                             mock layer; distilled from today's `tests/mocks.rs`
                             building blocks)

tools_core
├── lib.rs
├── tool.rs                 (Tool trait, ToolContext with extensions)
├── dyn_tool.rs             (DynTool, AnyOutput)
├── registry.rs             (ToolRegistry — instance instead of singleton)
├── spec.rs                 (ToolSpec, capability tags)
├── render.rs               (Render, ResourcesTracker, ImageData)
├── result.rs               (ToolResult, ToolError)
└── title.rs                (title templating)
   # The core only knows the abstract ToolDialect trait (see §3.7) plus the
   # native default in agent_core. The XML/Caret implementations live in
   # code_assistant_core.

code_assistant_core       (NEW: domain layer, uses the crates above)
├── tools/                  (impls, registered in its own registry instance)
├── tool_dialects/          (NEW: one directory per dialect as a "vertical slice")
│   ├── mod.rs              (selection helper: ToolSyntax → Box<dyn ToolDialect>;
│   │                        ToolSyntax::Native yields the core default impl)
│   ├── xml/                (parser.rs, formatter.rs, stream.rs, prompt_docs.rs, tests.rs)
│   └── caret/              (same — Native lives as the default in the core, see agent_core)
├── plugins/                (NEW: code-assistant-specific hooks,
│                            tests as #[cfg(test)] mod in the same file)
│   ├── plan.rs             (plan tool hook)
│   ├── name_session.rs     (naming reminder + special tool)
│   ├── projects.rs         (file trees + AGENTS.md in the system prompt)
│   ├── compaction.rs       (threshold + prompt)
│   ├── prompt_too_long.rs  (recovery strategy)
│   └── sub_agent.rs        (sub-agent plugin)
├── ui_events.rs            (domain UiEvent — embeds AgentUiEvent, see §1.1/§3.8;
│                            plus generic streaming parts like DisplayFragment)
├── session/                (branching, SessionInstance, manager, persistence)
└── ...

ui_gpui                   (NEW: GPUI frontend — today's ui/gpui/)
ui_terminal               (NEW: terminal frontend — today's ui/terminal/)

code_assistant            (exists, shrinks to the wiring binary)
├── cli/                    (argument parsing)
├── app/                    (assembly: build runtime via builder, pick frontend)
└── main.rs
```

The exact split can also start as a single `agent_core` crate with submodules and be
split into multiple crates later.

**Layout principles** (they fix the two main navigation problems of the current state):

1. **Dialects as vertical slices instead of horizontal layers.** Today one dialect is
   smeared across two trees: parsing/formatting/prompt docs under `tools/`
   (`parse.rs`, `formatter.rs`, `parser_registry.rs`, `system_message.rs`) and the
   stream processors under `ui/streaming/`. Anyone wanting to understand "how does
   Caret work?" has to read four files in two directories today. In the target picture,
   everything belonging to one dialect lives in *one* directory, including its tests.
2. **Tests live with the code they test.** Today `agent/tests.rs` (~2,700 lines, also
   containing parser tests) and `tools/tests.rs` (~1,300 lines) accumulate
   cross-cutting content; `tests/` additionally holds mocks and integration tests. In
   the target picture: dialect tests with the dialects, plugin tests with the plugins,
   loop tests with the runner, and `tests/` shrinks to genuine integration tests plus
   the (shrinking) code-assistant-specific mocks. Generic mocks (LLM provider, UI,
   persistence) move into the core as `test_utils`.

### 3.2 Generic `Agent` core

The central type is decoupled from application state:

```rust
// agent_core::agent::runtime
pub struct AgentRuntime<E: AgentExtensions> {
    llm: Box<dyn LLMProvider>,
    tools: Arc<ToolRegistry<E::ToolExt>>,
    dialect: Arc<dyn ToolDialect>,        // replaces parser + formatter (see §3.7)
    ui: Arc<dyn AgentUi>,
    state: Box<dyn StatePersistence<Snapshot = E::Snapshot>>,
    permissions: Option<Arc<dyn PermissionMediator>>,
    hooks: HookRegistry<E>,
    config: AgentConfig,
    session: SessionContext,             // generic, small container
    extensions: E::State,                // app-specific state
}
```

`AgentExtensions` is a trait implemented by the consumer that bundles all variation
points:

```rust
pub trait AgentExtensions: Send + Sync + 'static {
    /// Application-specific state that hooks may read/write.
    type State: Send + Sync;

    /// Application-specific tool context slice (see §3.4).
    type ToolExt: ToolContextExtension;

    /// Persisted state (today's `SessionState` fields, as needed).
    type Snapshot: Serialize + DeserializeOwned + Send + Sync;
}
```

### 3.3 Generic agent loop

Today `Agent::run_single_iteration` grinds through ~100 lines of special cases. The goal
is to reduce this loop to a simple, deterministic skeleton that plugins hook into.
Pseudocode:

```rust
loop {
    hooks.before_iteration(ctx).await?;

    // 1. Append pending user message, if any
    hooks.collect_pending_user_input(ctx).await?;

    // 2. Ask the compaction policy
    if hooks.compaction_policy(ctx)?.should_compact() {
        hooks.run_compaction(ctx).await?;
        continue;
    }

    // 3. Render phase: dynamically replace tool results, inject reminders ...
    let mut request = ctx.build_llm_request();
    hooks.shape_request(&mut request, ctx).await?;

    // 4. LLM call (with retry hook)
    let response = ctx.send_request(request, hooks.retry_policy()).await?;
    hooks.observe_response(&response, ctx).await?;

    // 5. Tool extraction (delegated to the parser)
    let (tool_requests, flow) = hooks.extract_tools(&response, ctx)?;

    // 6. Special-tool dispatch
    if let Some(decision) = hooks.intercept_tools(&tool_requests, ctx).await? {
        match decision { Break => return, GetUserInput => return, Continue => continue, ... }
    }

    // 7. Tool execution (parallel or sequential, decided by the hook)
    let results = hooks.execute_tools(tool_requests, ctx).await?;

    // 8. Results back into the state
    hooks.record_tool_results(results, ctx).await?;

    hooks.after_iteration(ctx).await?;
}
```

The `hooks.*` calls above form the **stable contract** between core and consumer.
Defaults in the core behave "neutrally" (no special behavior), so a minimal consumer
does not have to write any plugin at all.

### 3.4 Generic `ToolContext`

Instead of a monolithic struct, the tool context becomes a "service locator" with
type-safe extensions:

```rust
// tools_core::tool
pub struct ToolContext<'a, Ext: ToolContextExtension = ()> {
    pub command_executor: &'a dyn CommandExecutor,
    pub ui: Option<&'a dyn AgentUi>,
    pub tool_id: Option<&'a str>,
    pub permissions: Option<&'a dyn PermissionMediator>,
    pub cancel: &'a CancellationToken,
    pub ext: &'a mut Ext,                 // application-specific
}

pub trait ToolContextExtension: Send {
    /// Optional: lookup of specific sub-services by TypeId.
    fn get<T: 'static>(&self) -> Option<&T> { None }
    fn get_mut<T: 'static>(&mut self) -> Option<&mut T> { None }
}
```

`code-assistant` then defines, once:

```rust
struct CaExt {
    project_manager: Box<dyn ProjectManager>,
    plan: Option<PlanState>,
    sub_agent_runner: Option<Arc<dyn SubAgentRunner>>,
    session_id: Option<String>,
    // ...
}
impl ToolContextExtension for CaExt { ... }
```

and its tools expect `ToolContext<'_, CaExt>`. Tools of other applications see their own
extension. `CommandExecutor` and `PermissionMediator` stay in the core because they are
already generic.

Alternative design: a heterogeneous service locator via `AnyMap`/`TypeMap`. The typed
approach is safer; the `AnyMap` approach opens the plugin architecture up further.

> **Note:** The `cancel: &CancellationToken` field in the sketch is *new*
> functionality, not extraction. Today, cancellation runs through
> `ui.should_streaming_continue()` and (for sub-agents) the
> `SubAgentCancellationRegistry`. Leave it out for the extraction initially and keep
> the existing behavior; a token can be added later.

### 3.5 Hooks / plugins (the heart of it)

The most important extension points as small traits whose default implementations are
"passthrough".

> **Typing decision:** The hook traits must either (a) be generic over
> `E: AgentExtensions` (`trait ToolInterceptor<E>`, stored as
> `Box<dyn ToolInterceptor<E>>` in the `HookRegistry<E>`) so hooks can access
> `ctx.extensions: &mut E::State` in a type-safe way, or (b) the `LoopCtx` exposes the
> app state only as `&mut dyn Any`. The sketches below omit the parameter for
> readability — option (a) is the recommendation; it does infect the registry, builder,
> and all hook definitions with the generic, but it stays object-safe and compiles
> without downcasts. (Example §5.3 already implements against `LoopCtx<'_, CaExt>`.)

```rust
/// Participates in building the system prompt.
#[async_trait]
pub trait SystemPromptProvider: Send + Sync {
    async fn build(
        &self,
        ctx: &PromptContext<'_>,
    ) -> Result<String>;
}

/// Pre-/post-iteration hooks (logging, injecting reminders, ...).
#[async_trait]
pub trait IterationHook: Send + Sync {
    async fn before_iteration(&self, ctx: &mut LoopCtx<'_>) -> Result<()> { Ok(()) }
    async fn shape_request(
        &self,
        request: &mut LLMRequest,
        ctx: &mut LoopCtx<'_>,
    ) -> Result<()> { Ok(()) }
    async fn observe_response(
        &self,
        response: &LLMResponse,
        ctx: &mut LoopCtx<'_>,
    ) -> Result<()> { Ok(()) }
    async fn after_iteration(&self, ctx: &mut LoopCtx<'_>) -> Result<()> { Ok(()) }
}

/// Intercept special tool names (complete_task, name_session, …).
#[async_trait]
pub trait ToolInterceptor: Send + Sync {
    /// Called before the standard execution.
    /// Returning `Some(_)` replaces the standard execution.
    async fn try_handle(
        &self,
        tool: &ToolRequest,
        ctx: &mut LoopCtx<'_>,
    ) -> Result<Option<InterceptOutcome>>;
}

pub enum InterceptOutcome {
    /// Tool has been handled; the ToolResult is optional.
    Handled { result: Option<Box<dyn AnyOutput>>, hidden_in_ui: bool },
    /// Tool should end the loop.
    BreakLoop,
    /// Tool should pause the loop and wait for the user.
    AwaitUser,
}

/// Strategy for parallel execution (today hardcoded to spawn_agent).
pub trait ToolDispatchPolicy: Send + Sync {
    fn partition<'r>(&self, requests: &'r [ToolRequest]) -> ToolBatchPlan<'r>;
}

/// Compaction policy.
pub trait CompactionPolicy: Send + Sync {
    fn should_compact(&self, snapshot: &ContextSnapshot) -> bool;
    fn compaction_prompt(&self) -> &str;
}

/// Retry/recovery policy (PromptTooLong, streaming errors, ...).
pub trait RecoveryPolicy: Send + Sync {
    fn classify(&self, err: &anyhow::Error) -> RecoveryAction;
}

pub enum RecoveryAction {
    Fail,                      // propagate the error
    RetryStream { delay: Duration },
    ReduceContext,             // delegates to `ContextReducer`
}

pub trait ContextReducer: Send + Sync {
    fn try_reduce(&self, ctx: &mut LoopCtx<'_>) -> Result<bool>;
}

/// Filter that decides which tool sequences are allowed in the stream.
pub trait ToolUseFilter: Send + Sync { ... }   // trait already exists

/// Persistent state, abstracted.
pub trait StatePersistence: Send + Sync {
    type Snapshot: Send + Sync;
    fn save(&mut self, snapshot: &Self::Snapshot) -> Result<()>;
    fn load(&self, id: &str) -> Result<Option<Self::Snapshot>>;
}
```

Hooks are collected in a `HookRegistry<E>` and set by the consumer at the start of
`AgentRuntime` construction, e.g. via a builder:

```rust
let runtime = AgentRuntimeBuilder::<CaExt>::new(llm, ui)
    .with_tools(my_tool_registry)
    .with_dialect(Box::new(CaretDialect::new()))
    .with_system_prompt(Box::new(CodeAssistantSystemPrompt::new(...)))
    .add_iteration_hook(Box::new(NameSessionReminderHook))
    .add_iteration_hook(Box::new(ProjectInfoHook))
    .add_tool_interceptor(Box::new(NameSessionInterceptor))
    .add_tool_interceptor(Box::new(UpdatePlanSnapshotInterceptor))
    .with_dispatch_policy(Box::new(SpawnAgentParallelPolicy))
    .with_compaction(Box::new(TokenRatioCompaction { threshold: 0.8, prompt: ... }))
    .with_recovery(Box::new(DefaultRecovery))
    .with_context_reducer(Box::new(DropLargestToolResults))
    .with_state_persistence(state_persistence)
    .build();
```

### 3.6 Generic `ToolRegistry` and `ToolSpec`

Instead of a global singleton, `ToolRegistry<Ext>` becomes instantiable and generic over
the tool context extension:

```rust
pub struct ToolRegistry<Ext: ToolContextExtension> {
    tools: HashMap<String, Box<dyn DynTool<Ext>>>,
}
```

`ToolScope` becomes free-form **capability tags**:

```rust
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters_schema: serde_json::Value,
    pub annotations: Option<serde_json::Value>,
    pub capabilities: &'static [&'static str], // e.g. "read_only", "edits_files"
    pub hidden: bool,
    pub title_template: Option<&'static str>,
}
```

Selection happens through freely combinable filter functions, e.g.
`registry.iter().filter(|t| t.has_capability("read_only"))`. This removes the hardcoded
`ToolScope` enum from the core.

`is_explore_tool` / `is_write_tool` / `SmartToolFilter` become pure consumer helpers
that evaluate the capability tags of the respective tools — without knowing tool names.

### 3.7 Tool invocation format as a plugin (text formats outside the core)

The agent core must not contain any XML or Caret specifics. It knows only abstract
`ToolRequest`s, abstract LLM responses, and an abstract stream processor for the UI —
plus one trivial built-in default for native tool calling (see below). The translation
between a concrete invocation format ("tool dialect") and these abstract concepts is
encapsulated in *one* small plugin trait:

```rust
// agent_core::tool_dialect

/// How a tool call travels between LLM and agent.
/// Implementations live in the consumer (e.g. `code_assistant::tool_dialects::xml`).
///
/// Object safety: the trait deliberately never takes `&ToolRegistry<...>` anywhere
/// (that would be a generic method → `Box<dyn ToolDialect>` impossible). Instead, the
/// caller passes pre-filtered `ToolSpec`/`ToolDefinition` slices. This also decouples
/// the dialect from the registry type.
pub trait ToolDialect: Send + Sync {
    /// Extract `ToolRequest`s from a completed LLM response.
    /// `order_offset` continues counting tools already extracted for this request
    /// (today's parser signature), so generated tool IDs stay unique.
    /// Additionally returns a variant of the response possibly truncated at the first
    /// tool position, so trailing text after a tool block does not end up in the
    /// transcript.
    fn extract_requests(
        &self,
        response: &LLMResponse,
        request_id: u64,
        order_offset: usize,
    ) -> Result<(Vec<ToolRequest>, LLMResponse)>;

    /// Format a `ToolRequest` back into this dialect's text representation. The core
    /// needs this for the format-on-save path (§2.1): when a tool changes its input
    /// during execution, the call is replaced in the message-history text (via
    /// `start_offset`/`end_offset`).
    fn format_tool_request(&self, request: &ToolRequest) -> Result<String>;

    /// Whether tool results travel to the API as native `ToolResult` blocks (native)
    /// or must be converted to text before the request (XML/Caret — today's
    /// `convert_tool_results_to_text`). The core can perform the conversion itself,
    /// since it knows the rendered tool outputs; the dialect only provides the
    /// decision.
    fn uses_native_tool_results(&self) -> bool;

    /// A stream processor that translates `StreamingChunk`s into `DisplayFragment`s.
    /// `hidden_tools` replaces the processors' current singleton access
    /// (`ToolRegistry::global().is_tool_hidden(name, ToolScope::Agent)` — the scope is
    /// even hardcoded there today): the caller passes in a predicate or a name set of
    /// the hidden tools.
    fn stream_processor(
        &self,
        ui: Arc<dyn AgentUi>,
        request_id: u64,
        hidden_tools: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    ) -> Box<dyn StreamProcessor>;

    /// How the dialect feeds the LLM tool list into the `LLMRequest`:
    /// - Native: `Some(tool_definitions)` — the LLM API knows the tools natively.
    /// - XML / Caret: `None` — the tools are described in the system prompt.
    fn populate_request_tools(&self, tools: &[ToolDefinition]) -> Option<Vec<ToolDefinition>>;

    /// Optional: block for the tool docs in the system prompt. `None` for native.
    fn render_tool_section_for_prompt(&self, tools: &[ToolDefinition]) -> Option<String>;

    /// Optional: format description ("this is how you call tools …"). `None` for native.
    fn render_format_section_for_prompt(&self) -> Option<String>;

    /// Detects whether an already stored message contains a tool invocation in *this*
    /// dialect (for normalization when loading the history).
    fn message_contains_invocation(&self, message: &Message) -> bool;
}
```

With this, today's `parser_registry` / `formatter` / `system_message` machinery for the
text formats lives in the consumer. `code_assistant` ships `XmlDialect` and
`CaretDialect` as internal implementations and picks one at runtime based on the session
configuration (`ToolSyntax`) — `ToolSyntax::Native` maps to the core's default
implementation.

What the core ships:

- The `ToolDialect` trait and the `StreamProcessor` trait (small, syntax-neutral).
- **Exactly one default implementation: native tool calling** (`dialect/native.rs`).
  It is trivial — pass `ToolUse` blocks through, `populate_request_tools` returns the
  tool definitions, no prompt docs, the stream processor is today's `json_processor` —
  and it is what practically every third-party consumer wants.
- The `AgentRuntimeBuilder` optionally accepts a `Box<dyn ToolDialect>`; without one,
  the native default applies.
- The `SystemPromptProvider` (see §3.5) receives the dialect and can ask it for
  `render_format_section_for_prompt` and `render_tool_section_for_prompt` when building
  the system prompt.

Consequently:

- **No `ToolSyntax` enum in the core.** It stays in `code_assistant` as the selection
  helper (CLI argument, session configuration, persistence) — the name is established
  and serialized there, and it is **not renamed**.
- **No global `ParserRegistry`** anymore. There is simply always exactly one dialect,
  set per agent instance.
- **No `tools_syntax` crate.** Today's XML/Caret implementations move as internal
  modules into `code_assistant` (under `tool_dialects/`). If someone really wants to
  share them, that can become an optional helper crate later — but it is explicitly not
  a mandatory part.
- **Tools themselves remain dialect-free.** They continue to know only their JSON schema
  and their `Render` output. `multiline_params`, schema `examples`, etc. are pure
  extra metadata for text dialects — a consumer staying on the native default can
  ignore them completely.

Implications for a few concrete cleanup items:

- `is_multiline_param` (today a hardcoded allow-list) is a detail of the XML/Caret
  dialect, not of the core. It may keep living in `code_assistant` — ideally
  data-driven there (e.g. a `multiline: true` field in the JSON schema, or a helper
  method on the `Tool` trait like `multiline_params() -> &'static [&'static str]`, so
  the list sits next to the tool instead of in a central place).
- The XML/Caret doc generators with their "magic placeholder names" (`project`, `path`,
  `regex`, …) are likewise pure dialect detail. Medium-term they should take their
  example values from `examples` in the JSON schema so the list no longer knows tool
  names — but that too happens in `code_assistant`, not in the core.
- `SerializedToolExecution::deserialize` accesses `ToolRegistry::global()` today; with
  the crate split it receives the `ToolRegistry<Ext>` as an argument (see §3.10). That
  is independent of the dialect topic.

### 3.8 Generic UI event set

The core contains only a **small, agent-centric** event set:

```rust
pub enum AgentUiEvent {
    StreamingStarted { request_id: u64, thread_node_id: Option<u64> },
    StreamingStopped { request_id: u64, cancelled: bool, error: Option<String> },
    RollbackStreaming { request_id: u64 },

    StartTool { tool_id: String, name: String },
    UpdateToolParameter { tool_id: String, name: String, value: String, replace: bool },
    UpdateToolStatus { tool_id: String, status: ToolStatus, message: Option<String>, output: Option<String>, ... },
    EndTool { tool_id: String },
    ToolOutputChunk { tool_id: String, chunk: String },

    AppendText(String),
    AppendThinking(String),
    AddImage { media_type: String, data: String },

    ReasoningSummaryStart,
    ReasoningSummaryDelta(String),
    ReasoningComplete,

    ShowTransientStatus(String),
    ClearTransientStatus,

    /// Application-specific events.
    Custom(Box<dyn Any + Send + Sync>),
}
```

Today's `UiEvent` variants for sessions, branching, worktrees, sandbox, drafts etc.
belong in the `code_assistant_core` layer. There the domain `UiEvent` **embeds** the
core events (`enum UiEvent { Agent(AgentUiEvent), Session(...), ... }`); the adapter
that implements the core's `AgentUi` trait wraps incoming core events into
`UiEvent::Agent(...)`, and the frontends (`ui_gpui`, `ui_terminal`) consume the one
concrete, fully-typed domain enum. `AgentUiEvent::Custom(Box<dyn Any>)` remains as an
escape hatch for third-party consumers only — our own frontends never downcast. This
keeps the `UserInterface` trait small and manageable for third-party applications.

### 3.9 Generic persistence

`StatePersistence` becomes generic over the snapshot type. The core provides a
"standard snapshot" with MessageTree + tool executions, extensible with
application-specific fields:

```rust
pub trait StatePersistence: Send + Sync {
    type Snapshot;
    fn save(&mut self, s: &Self::Snapshot) -> Result<()>;
    fn load(&self, id: &str) -> Result<Option<Self::Snapshot>>;
}

pub struct CoreSnapshot {
    pub session_id: String,
    pub message_tree: MessageTree,        // like today's MessageNodes/active_path
    pub tool_executions: Vec<ToolExecutionRecord>,
    pub next_request_id: u64,
    pub next_node_id: u64,
}
```

`code-assistant` sets its snapshot type to e.g.
`(CoreSnapshot, CodeAssistantSessionExt)`, and serialization happens in a wrapper
persistence adapter.

`ToolExecution::deserialize` is decoupled via a registry argument (instead of the
singleton).

### 3.10 Persisting tool outputs without the singleton

Today `SerializedToolExecution::deserialize` relies on `ToolRegistry::global()`. In the
target architecture, the loader must know the registry. Several options:

1. **Registry as argument**: `deserialize(&self, registry: &ToolRegistry<Ext>) -> ...`.
2. **Self-describing outputs**: `ToolOutput` carries enough type tagging to reconstruct
   via a deserializer map (output tag → `Box<dyn AnyOutput>` constructor).

Option 1 is the simplest and fits the general "away from singletons" direction.

### 3.11 Consumer view: what a minimal integration needs

The litmus test for the architecture: how does the core feel in a *foreign* project
that just wants to plug in its own tools? Target picture — three things are mandatory,
everything else has defaults:

```rust
// 1. Implement a tool — no syntax/dialect knowledge needed.
//    Required: Input (Deserialize), Output (Serialize + Render + ToolResult), spec(), execute().
struct QueryDatabaseTool;

#[async_trait]
impl Tool for QueryDatabaseTool {
    type Input = QueryInput;          // serde struct, describes the JSON schema
    type Output = QueryOutput;        // Render::render() = what the LLM sees as the result
    fn spec(&self) -> ToolSpec { /* name, description, parameters_schema, capabilities */ }
    async fn execute(&self, ctx: &mut ToolContext<'_, MyExt>, input: &mut QueryInput)
        -> Result<QueryOutput> { ... }
}

// 2. Fill a registry instance (no singleton, no default tools).
let mut tools = ToolRegistry::new();
tools.register(Box::new(QueryDatabaseTool));

// 3. Build the runtime — no dialect, no hooks, no interceptors needed.
let runtime = AgentRuntimeBuilder::new(llm_provider)
    .with_tools(tools)
    .with_system_prompt_text("You are a database assistant ...")
    .with_ui(my_ui)                   // or default: no-op UI for headless operation
    .build();
```

Notes:

- **Syntax is completely ignorable.** Without `with_dialect(...)`, native tool calling
  via the LLM API applies (§3.7). Tool implementations never touch the topic — dialects
  consume only the `ToolSpec` data. The same registry runs unchanged under XML/Caret if
  the consumer later sets a text dialect after all. Syntax and tools are therefore fully
  orthogonal in configuration too (separate builder calls).
- **`ToolContext` extension only when needed.** Tools that need no app services use
  `Ext = ()`. Anyone needing their own services (DB pool, custom config) defines an Ext
  type — that is the only place where the consumer touches the core's generics, as long
  as they write no hooks.
- **All remaining building blocks have neutral defaults:** hooks = passthrough,
  persistence = in-memory, compaction = off, recovery = generic streaming-retry
  heuristic, `PermissionMediator` = None.
- **Consequence for the core `ToolContext`:** `command_executor` should be `Option`
  there (or move into the extension) — a consumer without shell tools should not have
  to provide a `CommandExecutor` or pull in the crate dependency. Read the sketch in
  §3.4 accordingly.

What the consumer deliberately does *not* see: `ToolSyntax`, parsers, formatters,
stream processors for text formats, multiline/example metadata, capability scoping (as
long as they always offer all tools), and all code-assistant plugins.

---

## 4. Mapping today's special-case paths onto hooks

| Today (in `Agent`)                                   | Target                                       |
|------------------------------------------------------|---------------------------------------------|
| `complete_task` special case in `manage_tool_execution` | **delete after the test mocks are migrated** (see §2.1 and §6 Phase 1); `InterceptOutcome::BreakLoop` remains available as a hook option |
| `name_session` special case in `execute_tool`        | `ToolInterceptor::Handled { hidden_in_ui: true }` + `IterationHook::shape_request` for the reminder |
| `update_plan` → plan snapshot                        | `ToolInterceptor::after_success` variant / `IterationHook::after_iteration` |
| `spawn_agent` parallel with `mode=read_only`         | `ToolDispatchPolicy::partition` in a plugin  |
| `inject_naming_reminder_if_needed`                   | `IterationHook::shape_request`               |
| `convert_tool_results_to_text` (XML/Caret)           | core function, controlled via `ToolDialect::uses_native_tool_results()` (§3.7) |
| Format-on-save (`update_message_history_with_formatted_tool`, `notify_tool_parameter_updates`) | core function; uses `ToolDialect::format_tool_request` (§3.7) |
| `render_tool_results_in_messages` (synthetic cancellations) | core function; stays generic             |
| `is_prompt_too_long_error` + `replace_large_tool_results` | `RecoveryPolicy` + `ContextReducer`      |
| `is_retryable_streaming_error`                       | `RecoveryPolicy::classify`                   |
| `should_trigger_compaction` + `perform_compaction`   | `CompactionPolicy` + `ContextReducer::compact` |
| `read_guidance_files` (AGENTS.md/CLAUDE.md)          | `SystemPromptProvider` plugin                |
| `init_projects` / `file_trees` / `available_projects`| `SystemPromptProvider` plugin                |
| `cached_system_prompts` / model mapping              | `SystemPromptProvider` implementation        |
| `tool_scope` / diff-blocks variant                   | capability tags + configuration flag         |
| `pending_message_ref`, `update_activity_state`,
  `build_current_metadata`, `save_state` →
  `ChatMetadata` update                               | stays in `code_assistant` (via `IterationHook::after_iteration` and a `SessionExtension`); the core only holds the `MessageTree` snapshot |
| Sub-agent output JSON format (`SubAgentOutput`)      | stays in `code_assistant` as tool output     |
| Branching (`MessageNode`, `active_path`, …)          | optionally as a `branching` feature in the core |

---

## 5. Concrete type sketches

> These sketches are intentionally rough. They are meant to show how today's structures
> translate into the generic building blocks.

### 5.1 `AgentConfig`

```rust
pub struct AgentConfig {
    pub max_streaming_retries: u32,
    pub streaming_retry_base_delay: Duration,
    pub default_tool_syntax: SyntaxId,
    pub max_iterations: Option<u32>,         // optional; None = unlimited
    // NO model_hint, NO sandbox_policy, NO init_path, NO initial_project — those
    // are code-assistant concepts and belong in its extension.
}
```

### 5.2 `LoopCtx`

```rust
pub struct LoopCtx<'a, E: AgentExtensions> {
    pub messages: &'a mut MessageTree,
    pub tool_executions: &'a mut Vec<ToolExecutionRecord>,
    pub ui: &'a dyn AgentUi,
    pub llm: &'a dyn LLMProvider,
    pub dialect: &'a dyn ToolDialect,
    pub tool_registry: &'a ToolRegistry<E::ToolExt>,
    pub permissions: Option<&'a dyn PermissionMediator>,
    pub session: &'a SessionContext,
    pub extensions: &'a mut E::State,
    pub config: &'a AgentConfig,
}
```

### 5.3 Example: `NameSessionInterceptor`

```rust
struct NameSessionInterceptor;

#[async_trait]
impl ToolInterceptor for NameSessionInterceptor {
    async fn try_handle(
        &self,
        tool: &ToolRequest,
        ctx: &mut LoopCtx<'_, CaExt>,
    ) -> Result<Option<InterceptOutcome>> {
        if tool.name != "name_session" { return Ok(None); }

        let title = tool.input.get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing title"))?;

        ctx.extensions.session_name = title.to_string();
        let output = NameSessionOutput { title: title.into() };

        Ok(Some(InterceptOutcome::Handled {
            result: Some(Box::new(output)),
            hidden_in_ui: true,
        }))
    }
}
```

### 5.4 Example: `CodeAssistantSystemPrompt`

```rust
struct CodeAssistantSystemPrompt {
    base_prompts: PromptMapping,
    tool_intro: &'static str,
    project_manager: Arc<dyn ProjectManager>,
    cache: Mutex<HashMap<String, String>>,
}

#[async_trait]
impl SystemPromptProvider for CodeAssistantSystemPrompt {
    async fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        // 1) Select the model-specific base prompt
        // 2) Get syntax doc + tool doc from the parser
        // 3) Append project file trees & AGENTS.md
        // ... like today, but encapsulates the entire code-assistant part.
    }
}
```

### 5.5 Example: `SpawnAgentParallelPolicy`

```rust
struct SpawnAgentParallelPolicy;

impl ToolDispatchPolicy for SpawnAgentParallelPolicy {
    fn partition<'r>(&self, reqs: &'r [ToolRequest]) -> ToolBatchPlan<'r> {
        let (parallel, sequential) = reqs.iter().partition(|r| {
            r.name == "spawn_agent"
                && r.input.get("mode").and_then(|v| v.as_str()).unwrap_or("read_only") == "read_only"
        });
        ToolBatchPlan { parallel, sequential }
    }
}
```

### 5.6 Example: `TokenRatioCompaction`

```rust
struct TokenRatioCompaction {
    threshold: f32,
    prompt: &'static str,
    context_limit: Box<dyn Fn(&SessionContext) -> Option<u32> + Send + Sync>,
}

impl CompactionPolicy for TokenRatioCompaction { ... }
```

---

## 6. Migration plan in phases

The conversion can be carried out in several steps without the application being broken
in between:

### Phase 1 — Introduce hook points, without a crate split

0. Remove the `complete_task` path (see §2.1) — as an upfront commit. An analysis of
   `agent/tests.rs` shows the tests already fall into two groups:
   - **Tests that already end on a text response today** (the compaction tests, the
     prompt-too-long test, `test_write_file_outside_root_…` with its explicit
     `completion_response`): for these, the auto-inserted `complete_task` response is
     **never consumed** — it sits as dead weight at the bottom of the response stack.
     These tests prove that the natural loop ending ("response without tools →
     GetUserInput") works as a test terminator. No change needed.
   - **Tests that end after a tool execution** (`test_unknown_tool_…`,
     `test_invalid_xml_…`, `test_parse_error_…`, …): here the auto-inserted response
     actually serves as the loop terminator. These tests get an explicit final text
     response (`create_test_response_text("…")`) appended to their response list.

   Recommendation therefore: **remove the auto-insert in `MockLLMProvider::new`
   entirely** rather than converting it to a text response — every test declares its
   complete response sequence itself. The magic insert costs comprehension today
   (comments like "popped last before complete_task" in the tests prove it), and the
   mock already fails loudly on exhausted responses ("No more mock responses"), so a
   forgotten terminator is immediately visible. Assertions on `requests.len()` remain
   valid unchanged (same number of LLM calls); only the last assistant message in the
   history is then text instead of a dangling `ToolUse`.

   Afterwards, delete the path in `manage_tool_execution` (plus the `complete_task`
   entry in `ui/gpui/shared/file_icons.rs`).
1. Extract the special-case paths from `Agent::run_single_iteration` into private
   methods: `intercept_special_tool`, `apply_naming_reminder`,
   `partition_parallel_tools`, `compaction_policy`, `recovery_policy` — and isolate the
   format-on-save path (`update_message_history_with_formatted_tool` +
   `notify_tool_parameter_updates`) as its own unit. Behavior stays the same.
2. Replace global `ToolRegistry::global()` calls in parser/persistence with an
   injectable argument (a compatibility wrapper that keeps using `global()` remains for
   now). The same applies to the calls in the stream processors and `title.rs` (§2.3) —
   there, an injected "is hidden" predicate suffices initially instead of the whole
   registry.
3. Extend `ToolContext` with an `extensions: &mut dyn Any` backdoor so tools that could
   already be more generic (`read_files`, `web_search`, …) no longer need a
   `ProjectManager` reference.

### Phase 2 — Introduce plugin traits

1. Create the traits `IterationHook`, `ToolInterceptor`, `ToolDispatchPolicy`,
   `CompactionPolicy`, `RecoveryPolicy`, `ContextReducer`, `SystemPromptProvider`,
   `StatePersistence` as modules under `crates/code_assistant/src/agent/hooks/`.
2. Move the existing special-case paths into plugin implementations
   (`PlanSnapshotHook`, `NameSessionInterceptor`, `CodeAssistantSystemPrompt`, …).
3. `Agent::run_single_iteration` only calls through the hook registry. Tests stay the
   same.

### Phase 3 — `ToolScope` → capabilities, `is_multiline_param` → schema-driven

1. Extend `ToolSpec` with `capabilities: &'static [&'static str]` (parallel to
   `supported_scopes`).
2. Switch `SmartToolFilter` and the sub-agent logic to capabilities. While at it, adapt
   the `SubAgentRunner::run` signature (it takes `tool_scope: ToolScope` today, see
   §2.9) and dissolve the hardcoded `ToolScope::Agent` assumption in the stream
   processors.
3. Derive multiline parameters and doc examples from the JSON schema; remove
   `is_multiline_param` and the "magic placeholder names".
4. Remove the `ToolScope` enum or reduce it to `enum ToolScope(&'static str)`
   (just-a-tag). This includes the MCP handler (`ToolScope::McpServer` → capability
   filter, §2.12).

### Phase 4 — Crate split

1. Create the new crate `tools_core`, moving there:
   `tools/core/{tool, dyn_tool, registry, render, result, spec, title}.rs`.
2. New crate `agent_core`: `agent/runner.rs`, the new hook modules, `MessageTree`, the
   `AgentUiEvent` minimum, the `StatePersistence` trait, the abstract `ToolDialect`
   trait and `StreamProcessor` trait, plus the **native default implementation**
   (including today's `json_processor` as its stream processor) — *but no XML/Caret
   implementations*.
3. `code_assistant` (interim — the content moves on to `code_assistant_core` in
   Phase 5):
   - Moves `tools/{parse, formatter, parser_registry, system_message}.rs` and
     `ui/streaming/{xml,caret}_processor.rs` into an internal module `tool_dialects/` —
     organized **per dialect** (`xml/`, `caret/`, see layout principles in §3.1) — and
     implements `ToolDialect` + `StreamProcessor` for XML and Caret there.
     (`json_processor.rs` moves into the core as part of the native default
     implementation, see step 2.) The associated tests (`*_processor_tests.rs`, the
     `tools/tests.rs` portions, parser tests from `agent/tests.rs`) move along. These
     implementations stay application-internal.
   - `tool_use_filter.rs` (`SmartToolFilter`) — after the refactoring it evaluates
     capability tags instead of tool names (cf. Phase 3).
   - Implements `AgentExtensions` (`CaExt`, snapshot, ToolExt).
   - Switches the MCP handler to the registry instance and the new `ToolContext`
     (with `CaExt`) — it is the second in-process consumer (§2.12).
   - Keeps branching, sub-agents, sessions, persistence files.
4. Optional: extract `agent_persistence` with a JSON file adapter.

### Phase 5 — Domain layer and frontend crates

1. Create `code_assistant_core` and move there: `session/`, `persistence.rs`, the tool
   impls (`tools/impls/`), `tool_dialects/`, `plugins/`, sub-agents, the permission
   code, and the domain `UiEvent` (restructured to embed `AgentUiEvent`, see §3.8).
2. Create `ui_gpui` from `ui/gpui/` and `ui_terminal` from `ui/terminal/`. Both depend
   on `code_assistant_core` (for `UiEvent`, session/persistence types) — never the
   other way around. Whatever `agent/`/`tools/` still referenced from `ui/` at this
   point must already live below (the `UserInterface` trait in `agent_core`, the
   events in `code_assistant_core`).
3. `acp/` and `mcp/` either stay as modules in `code_assistant_core` or move into the
   binary — carve them out as crates only when something forces it (ground rule §1.2.3).
4. `code_assistant` shrinks to the wiring binary: CLI parsing, config loading, builder
   assembly, frontend selection (feature-gated, so e.g. a headless build skips gpui).

### Phase 6 — Cleanup

1. Remove `ToolRegistry::global()`; all callers receive the registry by argument or via
   the `ToolContext`/`LoopCtx`. Besides the agent loop and the parser, the caller list
   includes: the stream processors, `title.rs`, `formatter.rs`, the MCP handler, and
   `SerializedToolExecution::deserialize`. Likewise dissolve `ToolsConfig::global()` —
   the availability configuration is passed when filling the registry instance.
2. Drop the `ParserRegistry` singleton without replacement — per agent there is exactly
   one `Box<dyn ToolDialect>`, set at build time. The `ToolSyntax` enum disappears from
   the core; in `code_assistant_core` it may remain as the internal selection helper
   for the bundled dialects.
3. Move the sub-agent-specific UI adapters into `code_assistant_core`; only the
   `SubAgentRunner` trait remains in the core — and even that is optional.
4. Move the resources (`compaction_prompt.md`, `tool_use_intro.md`,
   `system_prompts/*.md`) into `code_assistant_core` (no default prompts in the core).

### Test migration (cross-cutting concern across all phases)

The existing tests are the refactoring's most important safety net — they must be
considered per phase, not at the end:

- **`agent/tests.rs` (~2,700 lines) is a mixed inventory** and should be disentangled
  during the refactoring:
  - The first ~365 lines (`test_flexible_xml_parsing`, `test_replacement_xml_parsing`,
    `test_mixed_tool_start_end`, `test_ignore_non_tool_tags`, …) are **pure XML parser
    tests** that don't construct an `Agent` at all — they belong with the dialect tests
    and move to `tool_dialects/` (Phase 4, or earlier as a free cleanup commit).
  - The rest drives the loop through the public `Agent` API with `MockLLMProvider`,
    `MockStatePersistence`, and a mock UI (predominantly `ToolSyntax::Native`, a few
    XML/Caret cases). Phases 1+2 leave this API unchanged — these tests remain as the
    regression net. The only upfront change: the `complete_task` terminator / the
    auto-insert in `MockLLMProvider::new` (see Phase 1, step 0).
- **`ui/streaming/{xml,caret,json}_processor_tests.rs` + `test_utils.rs`** are already
  cleanly separated per processor — in Phase 4 they move unchanged along with the
  processors to `tool_dialects/`.
- **Direct `ToolContext` construction** exists at exactly two test sites: the
  `#[cfg(test)]` constructor `ToolContext::new` and `tests/format_on_save_tests.rs`.
  Every `ToolContext` change (Phase 1.3 `dyn Any` backdoor, Phase 4 generic `Ext`)
  affects exactly these two places; keep the test constructor as the single entry point
  and evolve it along.
- **Unit tests inside the tool impls** (`read_files.rs`, `view_images.rs`,
  `view_documents.rs`, …) fetch `ToolRegistry::global()`. When switching to registry
  instances (Phase 4/5), they instead build a local registry with only the tools they
  need — a mechanical change and at the same time a win in test isolation (today all
  tests share the `OnceLock` state including `ToolsConfig`).
- **`tools/tests.rs` (parser/formatter tests, ~1,300 lines)** are effectively dialect
  tests: in Phase 4 they move together with the code to
  `code_assistant::tool_dialects/` (no rewriting, just moving + paths).
- **The `system_message.rs` tests** reference `ToolScope::Agent` and the
  `ParserRegistry` — they are adapted in Phase 3 (capabilities) and Phase 4 (dialect
  instead of registry) respectively.
- **`MockStatePersistence`** implements `AgentStatePersistence` against the concrete
  `SessionState`; when switching to the snapshot-generic `StatePersistence` trait
  (Phase 4) it becomes a generic in-memory adapter — at that point it sensibly belongs
  in the core (`agent_core`) as a test helper so third-party consumers can use it too.
- **Newly added:** isolated unit tests per plugin/hook (Phase 2) — this is part of the
  testability gain promised in §8 and should be written right when the special-case
  paths move, while the old behavior is still sitting next to it as a reference.

---

## 7. Open questions / design decisions

1. **Branching in the core or in the consumer?** The `MessageTree` model is not
   trivial, but not universal either. Proposal: offer it in the core behind a
   `branching` feature flag, with a linear default variant.
2. **`SubAgentRunner` in the core?** Sub-agents are a re-entry of the agent loop with
   their own state; the functionality is conceivable in the core, but the concrete UI
   adapter and output format are `code_assistant`-specific. Proposal: only an abstract
   trait in the core, everything else in the consumer.
3. **Static vs. dynamic tool capabilities.** Static `&'static [&'static str]` are cheap
   and probably sufficient; alternatively a bitset / type-safe capabilities.
4. **Persistence of `Box<dyn AnyOutput>`.** Today solved via tool name + registry. With
   the crate split, persistence must either know the consumer registry (passable) or
   tool outputs must carry their own tags.
5. **Streaming UI events vs. plugin events.** Some of today's UI events ("worktree
   update", "update plan") are triggered by the agent loop. They must travel via
   `AgentUiEvent::Custom`. That unifies the UI stream but costs some type safety.
6. **Synchronous vs. asynchronous hooks.** Some hooks (e.g.
   `ToolDispatchPolicy::partition`) run very frequently and should stay synchronous.
   Others (`CompactionPolicy::run`) must be async. The current sketch already makes
   this distinction.
7. **Multiple hooks of the same type.** Practically useful (composition!). Order must
   be deterministic; proposal: `IterationHook`s run in registration order,
   `ToolInterceptor`s in first-match-wins style.
8. **Naming — decided.** The `ToolSyntax` enum keeps its name and stays in
   `code_assistant` (CLI argument, session configuration, serialized fields — a rename
   would bring only migration costs). The new core trait is named `ToolDialect`,
   because it bundles more than syntax (parsing, back-formatting, streaming, prompt
   docs, request population) and "Native" has no text syntax at all. The two names
   coexist at the boundary: `ToolSyntax` is the consumer's configuration vocabulary,
   `ToolDialect` the core abstraction.
9. **Generics budget — leaning decided.** `AgentExtensions` with three associated
   types makes `AgentRuntime<E>`, `HookRegistry<E>`, `ToolRegistry<E::ToolExt>` and all
   hook traits generic (cf. the note in §3.5). That is type-safe, but the most
   expensive part of the design — and with the full breakup (§1.1) the generics would
   infect `code_assistant_core` and potentially the frontend crates. Decision lean:
   **trim aggressively before Phase 4.** Persist a fixed `CoreSnapshot` (§3.9) and
   leave app fields to the consumer's persistence adapter (`E::Snapshot` disappears),
   and prefer `dyn Any`-based extension state with one downcast helper in
   `code_assistant_core` over generic parameters on forty types. Phases 1–3 do not
   force any of these decisions yet.

---

## 8. What the refactoring investment delivers

- **Reusable core:** other applications (e.g. domain-specific assistants, test
  harnesses, MCP wrappers) can use the agent loop without a fork.
- **Clearer responsibilities:** special cases (plan, compaction, sub-agent, naming,
  recovery) live in their own modules instead of the 2,000-line `runner.rs`.
- **Better testability:** every hook is testable in isolation; the core loop no longer
  carries application-tool tests.
- **Clean extension of tool syntaxes:** third parties can add a format without patching
  the core.
- **Removal of the global registries:** multiple agents with different tool sets within
  one process become possible (today they all share one `OnceLock`).
- **Preparation for SDK delivery:** the core can be published as an external crate (or
  as a `cargo install agent-core` binary for a "headless agent SDK").
  (Publishing note: workspace-internal names like `tools_core` — and `llm`, `git`,
  `web` — are generic or taken on crates.io; publishing would mean renames, e.g. a
  prefix. As internal names they are fine and consistent with the house style.)
- **Faster builds, smaller binaries:** with `ui_gpui`/`ui_terminal` as separate crates,
  touching agent logic no longer recompiles the frontends, and headless builds can skip
  gpui entirely via feature gates.

---

## 9. Recommended order

1. Phases 1 + 2 (refactor, without crate split): high value, low risk, brings the
   architecture into a pluggable shape.
2. Phase 3 (ToolScope → capabilities, schema-driven defaults): medium effort, ends the
   hardcoded tool-name lists.
3. Phase 4 (generic crate split: `tools_core` → `agent_core`): mostly moving work;
   afterwards the core can be versioned separately.
4. Phase 5 (domain layer + frontends: `code_assistant_core` → `ui_gpui`/`ui_terminal`):
   mostly moving work once the trait/event layering is in place; each crate is a
   compilable checkpoint.
5. Phase 6 (cleanup): singleton removal, resource file moves, sub-agent separation.

Each phase is internally compilable, testable, and releasable. A big-bang refactoring
is not necessary.
