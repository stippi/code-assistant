# SessionService refactor — architecture review

Review of the `refactor/session-service` branch, which replaced the
`BackendEvent`/`BackendResponse` protocol (deleted `code_assistant_core/src/backend.rs`)
with a typed command facade, `SessionService`
(`crates/code_assistant_core/src/session/service.rs`).

## Context: the intended design (and where it diverged)

The refactor came out of a design conversation that split the messy UI↔core
conversation into **two directions across one seam**:

1. **Commands → typed `Result` reply.** Everything a frontend asks the core to
   *do* becomes a typed async method returning `Result<T>`, correlated per call
   (no matching by `session_id` + variant on a shared channel, no "no response
   is ambiguous" state). **This is what was implemented — and done well.**

2. **One broadcast `Event` stream.** Everything the core pushes *back* — agent
   streaming fragments, activity-state changes, metadata/plan updates,
   file-watcher refreshes — was to collapse into a single downstream `Event`
   stream that every frontend *subscribes* to and filters by `session_id`.
   **This half was deliberately not attempted.** The module doc states it:
   *"Core→UI notifications keep flowing through `UiEvent` and are not part of
   this API."*

Two consequences we predicted were meant to *fall out of* the broadcast stream,
and therefore did not happen:

- **"Connected" becomes a frontend-side filter**, letting ProxyUI's
  fragment/tool-status buffering and `is_ui_connected` gating be deleted.
- **ACP converges onto the same seam** instead of reimplementing session
  operations against `SessionManager`.

The fixed decisions we agreed on and that the implementation honors: keep the
`agent_core::AgentUiEvent` seam; a concrete handle (no external trait);
owned data in/out; commit-not-completion for agent-starting commands; defer any
local/remote backend abstraction *behind* the seam.

The net: this is a clean, well-executed **first half** (upstream command
unification). The **second half** (unified broadcast downstream → delete
ProxyUI buffering → migrate ACP) is the outstanding work.

## Better than intended

1. **The closure-actor eliminates the `Command` enum entirely.** The design
   debated wide `Command`/`Outcome` enums vs. typed methods. `call<T, F, Fut>`
   (`service.rs:172-191`) enqueues a boxed closure returning `Result<T>` and
   awaits a `oneshot`. There is no reified command vocabulary to leak or
   wildcard-match; each method reads top-to-bottom (lock → mutate → emit →
   return). Cleaner than the proposed Design C, and it sidesteps the open-bus
   "goes shallow" risk of Design B.

2. **Correlation and the "no response = error" concern are structurally
   closed.** A dead worker yields `"session service is not running"` /
   `"session service dropped the request"` (`service.rs:184-190`), with a test
   asserting it (`service_reports_stopped_worker`).

3. **Solves a problem the design missed:** the single worker decouples GPUI's
   non-tokio executor from the tokio runtime that must run the commands
   (`service.rs:151-170`, spawned at `crates/code_assistant/src/app/gpui.rs:64`).

4. **The seam is the test surface.** `backend.rs` had zero tests; `service.rs`
   ships a `#[cfg(test)]` module driving the real service over a temp dir +
   `MockUI` — the local-substitutable strategy discussed.

5. **Upstream cleanup is thorough.** The command-shaped `UiEvent` variants
   (`SendUserMessage`, `SwitchBranch`, `CancelSubAgent`, …) are removed from the
   enum. `backend.rs` (1852 lines) and `ui_gpui/src/app/backend.rs` (358) are
   deleted. Net −758 lines.

6. **Bonus — activity-state consolidation.** Activity-state rules were extracted
   from `ProxyUI` into a `SessionActivity` owner
   (`crates/code_assistant_core/src/session/instance.rs:52-134`); `ProxyUI`
   "owns no state logic" now. A locality win not planned in this thread.

## Where it diverged (the unfinished second half)

7. **Downstream is still `UiEvent` + `UserInterface::send_event`, not one
   broadcast `Event`.** A conscious scope cut. It leaves two downstream shapes:
   typed method returns *and* the `UiEvent` push stream.

8. **ProxyUI fragment/tool-status buffering and the `is_ui_connected` gate were
   kept** (`instance.rs`). The prediction was that an owned snapshot from
   `load_session` plus a broadcast every frontend filters would let this whole
   buffering class die. It survived, and the two are **causally linked**: as
   long as downstream is a single per-UI `send_event` push rather than a
   broadcast, the "connected" gate is still needed to stop a background
   session's fragments reaching the one attached UI. Correspondingly,
   `load_session` still fans out a burst of `UiEvent`s (`service.rs:206-238`)
   instead of returning a `SessionSnapshot`.

9. **ACP was not migrated — the strategic prize.** `AgentState` still holds
   `Arc<Mutex<SessionManager>>` and reimplements load/create/prompt/cancel plus
   its own polling wait loop (`crates/ui_acp/src/agent.rs`). `SessionManager`
   now has two callers: `SessionService` (gpui + terminal) and ACP directly, so
   resume/wait logic lives in two places. Most important follow-up.

10. **The model/sandbox "double-life" fan-out isn't implemented.**
    `switch_model` returns `ModelSwitchResult` to the caller but emits no
    broadcast for *other* views; `change_sandbox_policy` likewise just mutates
    and returns. Fine for today's single-connected-view world; it's the
    multi-view gap flagged in the design, and it can't be closed cleanly until
    (7) exists.

## Smaller notes

- **Global serialization → head-of-line blocking.** The worker awaits each
  command body to completion (`service.rs:164-167`); some bodies contain slow
  awaits — `create_llm_client_from_model(...)` in `start_agent_impl`, git I/O in
  `create_worktree`. Starting session A's agent can stall an unrelated
  `switch_model` on session B. Not a regression (the old single loop behaved the
  same), but not the cross-session concurrency the design intended.
- **Flat surface, no facets.** One `SessionService` with ~25 methods and section
  comments, rather than common-path methods + `branches()`/`git()` facets. Fine
  for a Rust handle; no change recommended.
- **Closures aren't reifiable / remote-compatible** — but that is an *internal*
  transport detail. The external seam is the typed methods, which stay
  transport-agnostic, so the "remote adapter later" door is still open (swap the
  method bodies or introduce a trait then, as planned).
- **`AGENTS.md:100-118` is now stale** — it still describes the deleted
  two-queue `BackendEvent`/`BackendResponse` architecture.

## Suggested follow-up (one coherent sequence)

Each step depends on the previous:

1. Introduce a unified broadcast `Event` stream for the downstream direction;
   have `load_session` return an owned `SessionSnapshot`.
2. Delete ProxyUI fragment/tool-status buffering and the `is_ui_connected`
   gate; "connected" becomes a frontend-side `session_id` filter, with
   resync-on-lag via `load_session`.
3. Migrate ACP onto `SessionService`, removing its direct `SessionManager`
   reimplementation and its bespoke wait loop.

Also: implement the model/sandbox double-life fan-out (step 1 enables it), and
update `AGENTS.md`.
