# Plan Tool Integration Plan

This document describes how to introduce a persistent "plan" tool to the code-assistant. The goal is to let the LLM maintain a structured plan (pending/in_progress/completed items), persist it per session, surface updates through all UIs, and forward changes to the Agent Client Protocol (ACP).

## Objectives

- Provide a hidden `update_plan` tool that the LLM can invoke with the full set of plan entries (content, priority, status).
- Persist plan state in sessions so reconnecting or switching sessions keeps the current plan.
- Deliver plan updates to terminal UI, GPUI, and ACP clients via new UI events.
- Ensure agents and tests remain backward compatible when no plan exists.

## High-Level Steps

1. Model the plan data alongside existing session state.
2. Extend tool infrastructure to expose plan access and add the new tool.
3. Broadcast plan updates through the UI event system and ACP bridge.
4. Adjust session activation to send the latest plan to newly connected UIs.
5. Update tests and docs to cover the new behavior.

---

## 1. Data Modeling & Persistence

**Add plan data structures**
- Introduce `PlanEntryPriority`, `PlanEntryStatus`, `PlanEntry`, and `PlanState` in `crates/code_assistant/src/types.rs`. Store plan items, optional metadata, and helper methods (e.g., `is_empty`).
- Keep them outside `WorkingMemory` and derive `Serialize`, `Deserialize`, `Clone`, and `Default` where appropriate.

**Persist plan with sessions**
- Extend `ChatSession` in `crates/code_assistant/src/persistence.rs` to include a `plan: PlanState` field (default to empty plan). Update `ChatSession::new_empty` accordingly.
- Mirror the plan on the in-memory side: add `plan: PlanState` to `SessionState` (`crates/code_assistant/src/session/mod.rs`) and ensure it is cloned when creating new session instances.
- Update `FileSessionPersistence::save_chat_session` / load code in `crates/code_assistant/src/persistence.rs` so plan state round-trips.
- When the agent saves state (`crates/code_assistant/src/agent/runner.rs`), include `self.plan.clone()` in the constructed `SessionState`. Similarly, when loading a session, pull plan data into the agent.

**Session Manager integration**
- In `crates/code_assistant/src/session/manager.rs`, make sure plan data is synchronized when `save_session_state` is called and when generating snapshots for inactive sessions.

## 2. Tool Infrastructure Changes

**ToolContext**
- Add `pub plan: Option<&'a mut PlanState>` to `ToolContext` in `crates/code_assistant/src/tools/core/tool.rs`. Maintain the `Option` pattern used for `working_memory` to keep existing call sites viable.
- Update all `ToolContext` constructors (agent runner, tests in `crates/code_assistant/src/tests/mocks.rs`, MCP handler) to provide either `Some(...)` or `None`.

**Plan tool implementation**
- Create `crates/code_assistant/src/tools/impls/update_plan.rs` implementing a hidden `update_plan` tool:
  - Input schema: full plan state (`entries: Vec<PlanEntryInput>` with content, priority, status).
  - Execution: validate entries, update the provided plan (fail gracefully if `context.plan` is `None`), emit a plan UI event (see Step 3), and return a summary string (e.g., "Plan updated: X items").
  - Output type implementing `Render`/`ToolResult` for the status message.
  - Unit tests using `ToolTestFixture` to verify plan persistence and UI signaling.

**Register the tool**
- Export it in `crates/code_assistant/src/tools/impls/mod.rs` and register in the default registry (`crates/code_assistant/src/tools/core/registry.rs`). Mark it hidden and scoped to `ToolScope::Agent`.
- Update smart tool filter in `crates/code_assistant/src/tools/tool_use_filter.rs` to treat `update_plan` as a "read" tool so the LLM can invoke it without blocking.

## 3. UI Event & ACP Wiring

**UI event definition**
- Add `UiEvent::UpdatePlan { plan: PlanState }` in `crates/code_assistant/src/ui/ui_events.rs`.
- Ensure `UiEvent::SetMessages` later includes plan data if needed. For now, a separate event keeps changes explicit.

**Agent <-> UI communication**
- Provide a helper on the agent to send plan updates:
  - When `update_plan` tool executes, call `self.ui.send_event(UiEvent::UpdatePlan { ... })`.
  - On agent load (in `Agent::load_session_state` or equivalent), after restoring plan data, emit an `UpdatePlan` so the active UI caches match.

**GPUI integration**
- In `crates/code_assistant/src/ui/gpui/mod.rs`, maintain an `Arc<Mutex<Option<PlanState>>>` similar to working memory. Handle the new `UiEvent::UpdatePlan` by updating cached state and triggering a refresh.
- Create a placeholder rendering slot (e.g., in the side panel) or leave a TODO comment referencing future visualization. The plan data just needs to be cached so later UI work can display it.

**Terminal UI integration**
- Extend `AppState` in `crates/code_assistant/src/ui/terminal/state.rs` to store an `Option<PlanState>`.
- Handle `UiEvent::UpdatePlan` in `crates/code_assistant/src/ui/terminal/ui.rs`, updating the state so future rendering work can surface it.

**ACP bridge**
- Update `crates/code_assistant/src/acp/ui.rs` to translate `UiEvent::UpdatePlan` into `acp::SessionUpdate::Plan`. Use the schema types from `agent-client-protocol` (`Plan`, `PlanEntry`, etc.).
- Ensure queued updates, ack handling, and error logging mirror the existing tool/status flows.

## 4. Session Activation & Persistence Hooks

**SessionInstance setup**
- When generating connect events (`crates/code_assistant/src/session/instance.rs`), push `UiEvent::UpdatePlan` with the stored plan after `UpdateMemory` so the UI syncs on session switch.
- When replaying history into the stream processor, no changes required unless plan data is included in message transcripts.

**Agent save/load**
- In `Agent::load_session_state` (or equivalent), set `self.plan = session_state.plan.clone();` and send `UpdatePlan` to the UI.
- In `Agent::save_state`, include the plan in `SessionState` to persist changes.

## 5. Testing & Documentation

**Unit tests**
- Add serialization round-trip tests in `crates/code_assistant/src/persistence.rs` (or a dedicated module) ensuring plans survive save/load.
- Extend `ToolTestFixture` in `crates/code_assistant/src/tests/mocks.rs` to support plan injection & inspection.
- Test the `update_plan` tool behavior (success, missing plan, input validation).
- Add an ACP unit test (if feasible) verifying `UpdatePlan` yields a `session/update` with `Plan` payload. If full integration is heavy, add TODOs and partial mocks for now.

**Docs**
- Update system prompts or user documentation describing when the LLM should update plans (e.g., reference `update_plan` in `crates/code_assistant/resources/system_prompts/default.md` once the tool exists).
- Document plan tool usage in `docs/plan-tool.md` (this file) and cross-link in README or CLAUDE instructions as needed.

**Follow-up**
- Later enhancements can include proactive reminders, UI rendering of plan items, and persistent priorities.

---

## Implementation Checklist

1. [ ] Types & persistence (`types.rs`, `persistence.rs`, `session/mod.rs`, `session/manager.rs`, `agent/runner.rs`).
2. [ ] ToolContext + fixtures (`tools/core/tool.rs`, `tests/mocks.rs`, `agent/runner.rs`, `mcp/handler.rs`).
3. [ ] `update_plan` tool module (`tools/impls/update_plan.rs`, registry entries, tool filter update).
4. [ ] UI event plumbing (`ui/ui_events.rs`, `ui/gpui/mod.rs`, `ui/terminal/state.rs`, `ui/terminal/ui.rs`).
5. [ ] ACP bridge update (`acp/ui.rs`).
6. [ ] Session connect events, agent save/load hooks (`session/instance.rs`, `agent/runner.rs`).
7. [ ] Tests for persistence, tool execution, ACP updates.
8. [ ] Prompt/docs updates.

Implementers should work through the checklist sequentially, running `cargo test` (workspace) after major milestones.
