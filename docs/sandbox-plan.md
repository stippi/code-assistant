# Sandbox Implementation Plan

This document captures a phased strategy for adding sandboxed execution and hardened
file-access controls to Code Assistant. The plan borrows proven ideas from
`codex-rs` (specifically the macOS seatbelt implementation in `core/src/seatbelt.rs`)
while accommodating Code Assistant’s multi-project model.

## Goals & Constraints

- Prevent tool calls (`replace_in_file`, `execute_command`, future memory tools) from
  touching files outside the set of user-approved project roots, including git-ignored
  or private paths.
- Support multiple simultaneous projects: the sandbox must cover the initially
  selected project plus any explicitly referenced `Project` the session opens later.
- Provide a permission-elevation path (LLM asks → user approves → sandbox widens or
  temporarily disables).
- Deliver a macOS implementation first (host machine), but keep the abstraction ready
  for Linux/Windows backends.
- Avoid breaking existing flows: sandbox-aware executors should fall back to the
  legacy behavior when running on unsupported platforms or when users explicitly opt
  out.

## Reference Notes from codex-rs

- **Policy Model**: `SandboxPolicy` enumerates `DangerFullAccess`, `ReadOnly`, and
  `WorkspaceWrite`. `WorkspaceWrite` carries explicit writable roots, optional
  network access, and flags for `/tmp`/`TMPDIR`. Each root can produce read-only
  subpaths (e.g., `.git`).
- **macOS Seatbelt (sandbox-exec)**:
  - Builds SBPL profiles on the fly, injecting writable roots and network clauses.
  - Uses canonicalized paths so `/var` vs `/private/var` do not diverge.
  - Spawns via `/usr/bin/sandbox-exec` with deterministic `-D` parameters.
  - Marks child env (e.g., `CODEX_SANDBOX` and `CODEX_SANDBOX_NETWORK_DISABLED`) so
    downstream processes know the constraints.
- **Permission Elevation Flow**: tool orchestration tracks approval policy +
  sandbox policy; if the sandbox denies a command, orchestration can retry outside
  the sandbox without another prompt (cached approval), or ask the user to grant a
  broader policy.

These ideas map well to Code Assistant: we already have session-level approval
settings and per-project metadata, so we can introduce analogous policies without
rewriting the entire agent loop.

## Crate Layout Considerations

Two directions looked plausible:

1. **Single “platform” crate** (e.g., `crates/platform/`) owning sandbox policy,
   file access helpers, command execution, encodings, etc. Pros: fewer crate edges
   and shared utilities stay colocated. Cons: the crate becomes a grab bag again,
   harder to test in isolation, and UI/application code still needs to pull in
   everything even if it only needs one piece.
2. **Focused crates**: `crates/sandbox/` for policies + seatbelt integrations,
   `crates/fs_explorer/` for CodeExplorer + file encoding logic, and
   `crates/command_executor/` for shell tooling. Pros: clearer layering, targeted
   dependencies, easier reuse by `llm`, `web`, or future crates. Cons: a bit more
   plumbing up front (additional Cargo crates and re-exports where needed).

Given the size of the existing `code_assistant` crate and our desire to harden each
layer independently, we’ll pursue option 2. If later we find recurring patterns
across the crates (e.g., shared path canonicalization helpers), we can introduce a
very small `crates/platform_utils/` with only those primitives.

## Proposed Architecture

1. **SandboxPolicy crate**: Mirror the codex types (trimmed to what we need) inside
   `crates/sandbox/`. This crate converts Code Assistant concepts (projects, temp
   roots, tool syntax) into platform-agnostic sandbox intents.
2. **SandboxContext Registry**:
   - Constructed per session; holds canonical project roots, optional writable globs,
     and read-only overrides (e.g., `.git`, `.env` if requested).
   - Provides helpers to test paths (`is_path_allowed(Path) -> Result<AllowedPath>`),
     used by `CodeExplorer` before any read/write operations.
   - Exposes `SandboxRequest` structures consumed by command executors.
3. **CommandExecutor Hierarchy** (in `crates/command_executor/`):
   - Keep `CommandExecutor` trait but add sandbox-aware implementations:
     - `SeatbeltCommandExecutor` (macOS): wraps `/usr/bin/sandbox-exec`, similar to
       codex’s `spawn_command_under_seatbelt`.
     - `PassthroughCommandExecutor`: current behavior, used when sandboxing is
       disabled.
   - Introduce a `SandboxedCommand` descriptor so the agent can declare whether a
     command needs read-only vs workspace-write access, plus required writable roots
     (derived from target project(s) + temp directories).
4. **CodeExplorer Hardening** (in `crates/fs_explorer/`):
   - Canonicalize all incoming paths relative to the project root.
   - Reject absolute paths and `../` escapes before hitting the filesystem.
   - When a project request references another project, merge its root into the
     registry so subsequent accesses succeed without loosening the sandbox globally.
5. **Permission Elevation**:
   - Extend the agent → UI protocol with a `SandboxPermissionNeeded` event that
     includes requested capability and blocking tool call.
   - Cache approvals at the session level to avoid repeated prompts (mirroring codex).
6. **Configuration & UX**:
   - CLI flags/env to select sandbox mode (`danger-full-access`, `read-only`,
     `workspace-write` w/ network toggle).
   - UI affordance to show active policy and allow manual override.

## Phased Implementation Plan

### Phase 0 – Scaffolding & Observability
- **Deliverables**
  - Introduce `sandbox` module with `SandboxPolicy`, `WritableRoot`, and helper
    enums/types copied/simplified from codex.
  - Add tracing around `CommandExecutor` and `CodeExplorer` to log attempted paths
    and whether sandbox enforcement is active.
  - Define error types (`SandboxViolation`, `SandboxUnavailable`).
- **Testing**
  - Unit tests for policy serialization/deserialization.
  - Snapshot tests ensuring writable-root derivation includes `.git` as read-only.

### Phase 1 – Path Validation for CodeExplorer
- **Deliverables**
  - Canonicalize project paths on load; store `ProjectSandboxScope { root, allowed_subprojects }`.
  - Update `Explorer::read_file`, `write_file`, `apply_replacements`, `list_files`, etc.
    to call `sandbox::ensure_within_scope(path)`.
  - Provide an allowlist of additional project roots gathered from the session’s
    `WorkingMemory.available_projects`.
- **Testing**
  - Unit tests using `tempdir` and `MockExplorer` to confirm attempts to escape via
    `../` or symlinks fail.
  - Integration test for multi-project session: ensure files inside the secondary
    project root remain accessible once added to the scope.

### Phase 2 – macOS Seatbelt CommandExecutor Prototype
- **Deliverables**
  - Port codex’s seatbelt launcher into `crates/code_assistant/src/sandbox/seatbelt.rs`
    with minimal dependencies (policy text embedded in `docs/` or `src/sandbox`).
  - Implement `SeatbeltCommandExecutor` that:
    - Accepts a `SandboxedCommandRequest` (command line, cwd, desired policy).
    - Builds SBPL policy with writable roots = union of selected project root(s)
      and optional `TMPDIR`.
    - Spawns via `/usr/bin/sandbox-exec`; falls back to `DefaultCommandExecutor`
      if the binary is missing or macOS APIs fail, emitting telemetry.
  - Provide feature flag / CLI toggle to opt into the prototype.
- **Testing**
  - Unit tests for SBPL argument generation (mirroring codex’s tests, e.g.,
    ensuring `.git` becomes read-only).
  - Integration tests guarded by `#[cfg(target_os = "macos")]` that spawn a simple
    command inside the sandbox and verify write attempts outside allowed roots fail.
  - Manual QA script (documented in `docs/sandbox-plan.md`) describing how to run
    `cargo test --package code-assistant sandbox::seatbelt::tests`.

### Phase 3 – Permission Elevation & UX Hooks
- **Deliverables**
  - Extend session state with `sandbox_policy: SandboxPolicy` and `approved_exceptions`.
  - Wire `CommandExecutor` and `CodeExplorer` errors into a user-facing prompt
    (“Command X needs write access to Y; allow once / allow for session / deny”).
  - Cache approvals; when granted, widen the sandbox scope (e.g., add a writable
    root) and retry the blocked operation automatically.
  - Persist the decision in session history so restarts reapply policies.
- **Testing**
  - UI-level tests (GPUI) to verify prompt rendering and branching decisions.
  - Agent tests ensuring a denied elevation surfaces a structured error to the LLM.
  - Property test for approval cache: repeated attempts should not re-prompt.

### Phase 4 – Tooling Integration & Multi-Platform Prep
- **Deliverables**
  - Update tool implementations (`replace_in_file`, `edit`, `write_file`, memory tools) to
    require explicit sandbox permissions (read-only vs write).
  - Introduce placeholders for Linux (`bwrap`/Landlock) and Windows (`AppLocker`)
    executors; even if unimplemented, keep trait boundaries so future work slots in.
  - Add telemetry counters for sandbox hits/misses, denials, and elevation requests.
- **Testing**
  - Regression tests around tool flows (e.g., editing git-ignored files should be
    blocked unless the user overrides).
  - Golden tests verifying tool transcripts mention sandbox outcomes (for ACP/MCP
    clients).

### Phase 5 – Hardening & Documentation
- **Deliverables**
  - Document sandbox behavior, configuration knobs, and limitations in `README.md`
    and a dedicated `docs/sandbox.md`.
  - Provide migration guidance for CLI users (flags, env vars).
  - Audit the `execute_command` tool to ensure streaming callbacks respect the
    sandbox (e.g., no direct `/bin/sh` bypass).
  - Extend fuzz/failure tests to simulate seatbelt denials, ensuring graceful
    fallback.
- **Testing**
  - End-to-end scenario tests covering:
    - Standard session staying within sandbox.
    - Multi-project editing.
    - User-approved elevation followed by successful command.
  - Nightly CI job (macOS runner) dedicated to seatbelt integration tests.

## Testing Strategy Summary

| Layer | Technique | Coverage |
| --- | --- | --- |
| Unit | Policy derivation, path validation, SBPL arg builders | Rust tests in `sandbox::*` modules |
| Integration | tempdir-based explorer tests & macOS seatbelt runner | `cargo test --package code-assistant sandbox::*` (mac target) |
| UI/Agent | GPUI component tests + agent workflow tests (existing mocks) | `crates/code_assistant/src/tests` additions |
| Manual | QA playbook for macOS seatbelt, documenting `sandbox` CLI flags | New section in `docs/sandbox.md` |
| Telemetry | Counters for denials/approvals validated via structured logs | Observed during CI + manual runs |

By sequencing the work through these phases, we can ship incremental safety
improvements (path validation) while building toward full sandbox parity with
codex-cli.
