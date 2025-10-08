# Config Consolidation Plan

## Context
- We discovered that configuration data for the core agent is duplicated across multiple structs.
- Static session information currently appears in `AgentRunConfig`, `AgentConfig`, and the flat fields stored on `persistence::ChatSession` (`init_path`, `initial_project`, `tool_syntax`, `use_diff_blocks`).
- Runtime LLM settings are duplicated between CLI inputs, `persistence::LlmSessionConfig`, and ad hoc fields like `model_hint` in `AgentLaunchResources`.
- Goal: keep existing type names when possible (`AgentConfig`, `SessionConfig`), reduce overlap, and enable runtime switching of provider/model in the future.

## Chosen Direction (Option 1)
1. Introduce a single `SessionConfig` (in `session/mod.rs`) that owns the static session fields:
   - `init_path: Option<PathBuf>`
   - `initial_project: String`
   - `tool_syntax: ToolSyntax`
   - `use_diff_blocks: bool`
2. Update `persistence::ChatSession` to store a `config: SessionConfig` instead of the individual fields. Provide backwards compatibility when deserializing existing session files.
3. Adjust `SessionState` to reference the new `SessionConfig` rather than separate fields.
4. Refactor `SessionManager`:
   - Its `AgentConfig` should become a thin wrapper holding `SessionConfig` plus any additional runtime wiring needed to build agents.
   - Methods that previously read `session.tool_syntax` / `session.use_diff_blocks` should read through `session.config`.
5. Update `Agent::new` and the agent runner to consume `SessionConfig` via `AgentConfig` / `AgentOptions`, eliminating the `init_path` and `tool_syntax` duplication. Diff-mode toggles should flow from `SessionConfig`.
6. Ensure UI layers (GPUI and Terminal) construct `SessionConfig` once from CLI arguments and pass it through `SessionManager`.
7. Clean up `model_hint` usage so LLM provider/model are taken from `LlmSessionConfig` only. Keep the ability to change LLM settings mid-session by persisting `LlmSessionConfig` separately.
8. Review persistence helpers (`FileStatePersistence`) and tests to adopt the new structure and to merge legacy fields when loading old JSON.
9. Update any call sites and tests that expect the old structure.

## Key Files
- `crates/code_assistant/src/session/mod.rs`
- `crates/code_assistant/src/session/manager.rs`
- `crates/code_assistant/src/agent/runner.rs`
- `crates/code_assistant/src/app/mod.rs`
- `crates/code_assistant/src/app/gpui.rs`
- `crates/code_assistant/src/app/terminal.rs`
- `crates/code_assistant/src/ui/terminal/app.rs`
- `crates/code_assistant/src/persistence.rs`
- `crates/code_assistant/src/agent/persistence.rs`
- Tests under `crates/code_assistant/src/tests/` that touch session persistence or agent configuration.

## Notes
- Keep the environment-compatible naming (`AgentConfig`, `SessionConfig`) but redefine responsibilities as above.
- Ensure backward compatibility when reading existing session JSON files by migrating the legacy fields into the new nested struct before use.
- After consolidation, no new logic should read `ChatSession::init_path` or similar legacy properties; `SessionConfig` becomes the single source of truth.
- Plan for a future step to expose an API for updating `LlmSessionConfig` mid-session.
