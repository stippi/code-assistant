# Sub-agents feature (`spawn_agent` tool)

## Implementation Status

### Completed
- [x] `SubAgentRunner` trait and `DefaultSubAgentRunner` implementation (`sub_agent.rs`)
- [x] `SubAgentCancellationRegistry` for managing cancellation tokens
- [x] `SubAgentUiAdapter` for streaming progress to parent tool block
- [x] `ToolScope::SubAgentReadOnly` and `ToolScope::SubAgentDefault` variants
- [x] Tool scope updates for read-only tools (search_files, read_files, glob_files, list_files, web_fetch, web_search, perplexity_ask)
- [x] `spawn_agent` tool implementation (`spawn_agent.rs`)
- [x] Tool registration in `mod.rs` and `registry.rs`
- [x] Session manager wiring of `DefaultSubAgentRunner` with UI and permission handler
- [x] Added required `Agent` methods: `set_tool_scope`, `set_session_model_config`, `set_session_identity`, `set_external_cancel_flag`, `message_history`
- [x] File reference enforcement with retry logic (up to 2 retries)

### Completed (phase 2)
- [x] **Terminal UI rendering**: Update terminal tool block to show streaming sub-agent activity
  - Added output rendering in `ToolWidget` with color coding for activity lines
  - Updated height calculation to account for multi-line output
- [x] **Parallel execution**: Update `manage_tool_execution()` to run multiple `spawn_agent` calls concurrently
  - Multiple `spawn_agent` read-only tools now run in parallel using `futures::join_all`
  - Results are collected in deterministic order matching original tool request ordering
  - Only `read_only` mode spawn_agents are parallelized for safety

### Completed (phase 3)
- [x] **Integration tests**: Added tests in `tests/sub_agent_tests.rs`:
  - `test_spawn_agent_output_render` - output rendering for success/cancel/error
  - `test_spawn_agent_input_parsing` - input parsing with defaults
  - `test_cancellation_registry` - cancellation registration and triggering
  - `test_mock_sub_agent_runner` - basic mock runner execution
  - `test_parallel_sub_agent_execution` - verifies concurrent execution
  - `test_tool_scope_for_sub_agent` - verifies tool availability per scope
  - `test_can_run_in_parallel_logic` - parallel execution eligibility logic


### Completed (phase 4)
- [x] **GPUI rendering**: Custom tool output renderer for sub-agent progress display
  - Added `ToolOutputRendererRegistry` pattern (similar to `ParameterRendererRegistry`)
  - Implemented `SpawnAgentOutputRenderer` that parses sub-agent activity markdown
  - Renders sub-tool calls in compact Zed-like style with icons and status colors
  - Located in `crates/code_assistant/src/ui/gpui/tool_output_renderers.rs`
- [x] **ACP mode support**: Sub-agent activity streams through existing tool output mechanisms
  - Added `spawn_agent` icon mapping in `file_icons.rs` (uses `rerun.svg`)
  - Output flows as `ToolCallUpdate` content for display in Zed's ACP panel

### Pending
- [ ] **UI integration for cancellation**: Expose cancel button per running `spawn_agent` block
- [ ] **Permission attribution**: Show permission requests as originating from sub-agent context (inline or popover)

### Notes
- The cancellation infrastructure is in place (`SubAgentCancellationRegistry`, `cancel` method, `cancel_sub_agent` helper) but the UI hooks to trigger cancellation are not yet implemented.
- The `sub_agent_cancellation_registry` field in `ToolContext` is available for future use when implementing tool-level cancellation from UI.

---

## Goal

Add a new tool, `spawn_agent`, that launches a sub-agent to execute a task with **isolated context/history**, returning only the **final output** back to the main agent as the tool result.

The primary motivation is **context window management**: the sub-agent can perform repetitive/exploratory work without polluting the main agent’s conversation history. Additionally, sub-agent progress should be visible in the UI **inside the `spawn_agent` tool block**, and multiple sub-agents should be able to run **concurrently**.

## UX requirements (what the user sees)

### During execution
- The main chat history shows a single tool call: `spawn_agent`.
- While the sub-agent runs, the tool block output is continuously updated with a reduced view of sub-agent activity, such as:
  - progress lines (e.g. “Searching…”, “Reading file…”) and/or
  - a compact list of the sub-agent’s tool calls.

This output is **UI-only** and should not be appended to the main agent’s message history. However, when persisting the `span_agent` tool execution, it allows for re-creating the custom tool output UI.

### Completion
- When the sub-agent completes, the `spawn_agent` tool block gets its final output.
- The tool result handed back into the main agent context is **only** the sub-agent’s final answer (or a cancellation message).

### Parallel sub-agents
- If the main agent requests multiple `spawn_agent` tool calls in a single response, they should run **truly concurrently**.
- Recommended safety default: only allow concurrent sub-agents in **read-only** mode.

### Cancellation
- Users can cancel a specific running sub-agent.
- The main agent sees a deterministic result: e.g. `"Sub-agent cancelled by user."`

### Permissions
- Sub-agents may use permission-gated tools.
- Permission requests should be attributed to the sub-agent (and ideally shown inline in the tool block, or as a popover anchored to it).

## Tool API

### Tool name
- `spawn_agent`

### Parameters
Keep parameters intentionally small:

- `instructions: string` (required)
- `require_file_references: bool` (optional, default `false`)
  - If true, the implementation appends a static instruction suffix telling the sub-agent to include exact file references with line ranges.
- `mode: "read_only" | "default"` (optional, default TBD)
  - `read_only` maps to a restricted tool scope.
  - `default` may allow broader tools (future/optional).

### Tool result returned to main agent
- For the main agent, the tool result content is the **final answer string**.
- If cancelled: return a cancellation string.
- Optional future extension: structured output (locations list), but not required for v1.

## File references (current code points likely to change)

The following files were identified during planning as the key implementation points:

### Tool registration & implementation
- `crates/code_assistant/src/tools/core/registry.rs`
  - Add registration for the new `spawn_agent` tool in `register_default_tools()`.
- `crates/code_assistant/src/tools/impls/mod.rs`
  - Add `pub mod spawn_agent;` and re-export `SpawnAgentTool`.
- New file: `crates/code_assistant/src/tools/impls/spawn_agent.rs`
  - Implement tool schema, invoke sub-agent runner, stream progress updates, and produce final tool output.

### Tool context / runtime plumbing
- `crates/code_assistant/src/tools/core/tool.rs`
  - Extend `ToolContext` to include a sub-agent spawning capability (e.g. `sub_agent_runner` or `agent_factory`).

### Agent loop changes (parallel tool execution)
- `crates/code_assistant/src/agent/runner.rs`
  - `manage_tool_execution()` currently executes tool requests sequentially.
  - Update it to run multiple `spawn_agent` tool calls concurrently (likely using `tokio::spawn` + join), while preserving deterministic ordering of tool-result blocks.
  - Add cancellation wiring hooks (tool-level cancellation signal).

### Session/UI integration (where agent is created)
- `crates/code_assistant/src/session/manager.rs`
  - This is where the main `AgentComponents` are built and the agent task is spawned.
  - Likely place to wire in factories/services needed by `spawn_agent` (e.g. LLM provider factory) and/or cancellation routing.

### Tool scopes
- `crates/code_assistant/src/tools/core/spec.rs`
  - Add new `ToolScope` variants:
    - `SubAgentReadOnly`
    - `SubAgentDefault`
- Tool implementations under `crates/code_assistant/src/tools/impls/*.rs`
  - Update each tool’s `ToolSpec.supported_scopes` to include/exclude the new sub-agent scopes.
  - Ensure edit/write/delete tools do **not** include `SubAgentReadOnly`.
  - The new `spawn_agent` tool includes neither scope, to prevent nesting agents.

### Permissions
- `crates/code_assistant/src/permissions/*` (exact files depend on current mediator implementation)
  - Ensure permission requests can be attributed to a sub-agent execution (ideally include parent tool id or sub-agent id in metadata).

### UI rendering for streaming sub-agent activity
- GPUI tool widget rendering:
  - `crates/code_assistant/src/ui/gpui/*` (tool widget / tool output renderer)
  - `crates/code_assistant/src/ui/terminal/tool_widget.rs` (terminal UI tool block rendering)

(Exact UI files depend on how tool outputs are currently rendered and updated; the runtime emits `UiEvent::UpdateToolStatus` in `crates/code_assistant/src/agent/runner.rs`.)

## Implementation approach

### 1) Add a sub-agent runner abstraction

Create an internal service (name flexible):
- `SubAgentRunner` (trait) or `AgentSpawner`

Responsibilities:
- Construct an in-process nested `Agent` configured for sub-agent operation.
- Ensure sub-agent state is isolated (no persistence into parent session).
- Provide a UI adapter that streams sub-agent activity into the parent `spawn_agent` tool block.
- Support cancellation via a per-sub-agent cancellation token.

### 2) LLM provider creation

Avoid requiring `LLMProvider: Clone`.

Implement a small factory interface used by the sub-agent runner:
- `LlmProviderFactory::create(model_name) -> Box<dyn LLMProvider>`

Use existing model configuration and construction:
- `crates/llm/src/factory.rs` provides `create_llm_client_from_model(...)`.

### 3) Tool scopes for sub-agent read-only mode

Add a new tool scope:
- `ToolScope::SubAgentReadOnly`

Update tools:
- Read/search/list/glob: include `SubAgentReadOnly`.
- Write/edit/delete/replace: exclude `SubAgentReadOnly`.

The sub-agent runner selects `tool_scope` based on `mode`.

### 4) `spawn_agent` tool implementation

In `crates/code_assistant/src/tools/impls/spawn_agent.rs`:

- Parse input parameters.
- Build final sub-agent instructions:
  - Always include `instructions`.
  - If `require_file_references`, append a fixed instruction block requesting references with line ranges.
- Ask runner to spawn sub-agent with:
  - chosen scope (`SubAgentReadOnly` for read-only mode),
  - cancellation token,
  - UI adapter target = the current tool id.
- Stream sub-agent activity into tool output.
- Return the final answer as the tool result content.

### 5) File reference enforcement (without rerunning)

If `require_file_references=true`:

- After sub-agent produces a candidate final answer, validate the presence of file references with line ranges.
- If missing, ask the *same* sub-agent to revise by appending a corrective user message.
- Bound number of retries (e.g. 2).
- If still missing, return a failure (or return best-effort answer with a warning — final behavior TBD).

This avoids rerunning from scratch and avoids involving the main agent in “please try again” loops.

### 6) Parallel execution of multiple `spawn_agent` tool calls

Update `Agent::manage_tool_execution()` (`crates/code_assistant/src/agent/runner.rs`):

- When tool requests include multiple `spawn_agent` calls, execute them concurrently.
- Keep deterministic ordering for the tool result blocks appended to the message history (match original tool request ordering).
- Recommended v1 safety: only run concurrently when `mode=read_only`.

### 7) Cancellation

- Each `spawn_agent` execution registers a cancellation handle keyed by the tool call id.
- UI exposes cancel control per running `spawn_agent` block.
- On cancellation:
  - cancel the sub-agent LLM request/tool loop,
  - mark tool status as cancelled,
  - return tool result text: `"Sub-agent cancelled by user."`

### 8) Permissions

- Sub-agent tool invocations may require permission.
- Ensure permission requests can be shown as originating from the sub-agent context.
- UX: inline within the tool block or popover.

## Testing plan

### Unit tests
- `spawn_agent` input validation.
- Rendering of tool result (only final output is returned to main agent).

### Integration tests
- Isolation: verify main agent message history contains only the `spawn_agent` tool result, not sub-agent transcript.
- Parallelism: multiple `spawn_agent` calls execute concurrently (use artificial delays/mocks).
- Cancellation: cancelling one sub-agent yields deterministic cancelled output and doesn’t cancel others.
- Permission routing: sub-agent permission prompts surface and block correctly.
- File reference enforcement: missing refs triggers in-sub-agent revision (bounded).

## Rollout notes

- Implement the UI streaming inside tool block first; ACP can initially show a markdown list.
- Keep the tool API minimal; add structured file reference extraction/enforcement later if needed.
