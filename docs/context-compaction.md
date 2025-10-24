# Context Compaction Implementation Plan

This document outlines the phased approach for adding automatic context compaction to the agent loop. The goal is to proactively summarize long conversations when the active model’s context window nears capacity, keep the UI history intact, and continue prompting the LLM with only the most recent summarized state.

## Phase 1 – Configuration & Data Model
- Require a `context_token_limit` field in every model entry in `models.json`.
- Update the configuration loader (`crates/llm/src/provider_config.rs`) and validation logic to deserialize, store, and surface this limit.
- Propagate the limit into `SessionModelConfig` (`crates/code_assistant/src/persistence.rs`) and ensure session creation (`crates/code_assistant/src/session/manager.rs`) records it.
- Introduce `ContentBlock::CompactionSummary { text: String }` in `crates/llm/src/types.rs` and adjust serialization, deserialization, and any exhaustive `match` arms that enumerate block variants.
- **Tests:** extend existing configuration loading tests (or add new ones) to assert `context_token_limit` is required and correctly parsed; add coverage verifying the new content block round-trips through serialization.

## Phase 2 – Agent Compaction Logic
- Add helpers in `crates/code_assistant/src/agent/runner.rs` to read the context limit, calculate the percent of the window consumed based on the latest assistant `Usage`, and define a compaction threshold (e.g., 80%).
- Before building an `LLMRequest` in `run_single_iteration`, detect when the threshold is exceeded.
- When triggered, inject a system-authored prompt requesting a detailed summary, send it to the LLM, and store the response as an assistant message containing `ContentBlock::CompactionSummary`.
- Adjust the message-preparation path (`render_tool_results_in_messages` and any related helpers) so the next LLM request only includes messages from the last compaction summary onward, while keeping the full `message_history` for persistence and UI.
- **Tests:** add unit coverage to assert the compaction branch fires when expected, the summary block is stored correctly, and filtering logic feeds only post-summary messages to the provider.

## Phase 3 – Persistence & Reload
- Ensure `ChatSession` serialization (`crates/code_assistant/src/persistence.rs`) handles the new summary block without data loss.
- Verify session loading (`Agent::load_from_session_state`) and `SessionInstance::convert_messages_to_ui_data` (`crates/code_assistant/src/session/instance.rs`) keep summaries visible while still allowing the agent to trim the prompt correctly.
- **Tests:** add persistence round-trip tests (if absent) that include a compaction summary and confirm reload semantics remain consistent.

## Phase 4 – UI Presentation
- Extend `DisplayFragment` with `CompactionDivider` in `crates/code_assistant/src/ui/streaming/mod.rs`.
- Update stream processors (`json_processor.rs`, `xml_processor.rs`, `caret_processor.rs`) to emit the divider fragment for `ContentBlock::CompactionSummary`.
- Enhance GPUI components:
  - Add a collapsible divider block in `crates/code_assistant/src/ui/gpui/elements.rs` showing the “conversation compacted” banner and the summary text.
  - Ensure `MessagesView` (`crates/code_assistant/src/ui/gpui/messages.rs`) handles the fragment, including expand/collapse state management.
- **Tests:** add GPUI/component tests (or logic tests where available) validating the divider renders, defaults to collapsed, and expands to reveal the summary.

## Phase 5 – Validation & Follow-Up
- Run formatting (`cargo fmt`), linting (`cargo clippy` once re-enabled), and targeted test suites (`cargo test` with focus on updated modules).
- Add or update documentation references pointing to this file if needed.
- **Tests:** confirm the new automated tests pass and consider adding integration coverage that simulates a full compaction cycle end-to-end.

