# Skills Feature – Implementation Plan

This document describes how to add an Anthropic-compatible **Skills** feature to
`code-assistant`. The design borrows the *progressive disclosure* model used by
both `codex-rs` and `vtcode`, while staying within the existing
architectural constraints of this codebase (in particular the
`'static`-string-only `ToolSpec`).

## 1. Goals & Non-Goals

### Goals

- **Spec compatibility.** Skills authored for Anthropic's Agent Skills spec
  (used by `codex-rs` and `vtcode`) should work in `code-assistant` without
  modification. Concretely: a `SKILL.md` with YAML frontmatter inside a
  directory, optionally bundled with `scripts/`, `references/`, `assets/`.
- **Progressive disclosure.** Only skill *metadata* (name + description +
  scope) is always present in the system prompt. The full body is loaded
  lazily, on demand.
- **Mode-agnostic.** The feature must work uniformly under `Native`, `Xml`,
  and `Caret` tool modes. No mode-specific code paths.
- **Multi-scope discovery.** Project, user, and bundled (system) skills, with
  predictable precedence on name collisions.
- **Sticky activation.** Once a skill is activated in a session, it stays
  active until the session ends (and across persistence boundaries), surviving
  context compaction.
- **Low blast radius.** Implementable in phases; phase 1 ships without any
  UI changes and without breaking existing sessions.

### Non-Goals (initial release)

- Dynamic, per-skill tool registration in the `ToolRegistry` (vtcode-style).
  Avoids a `&'static`-string refactor of `ToolSpec` and is not needed for the
  primary use case.
- Skill-declared MCP server installation (codex-rs `agents/openai.yaml`
  feature). Tracked under future scope.
- Remote/networked skill catalogs (codex-rs `hazelnuts` API).
- Automatic, fully-implicit invocation. The model decides whether to load a
  skill based on the catalog; we do not auto-load skills from text matching.

## 2. Skill Format

Skills follow Anthropic's `SKILL.md` spec. A skill is a directory whose name
matches the skill's `name`:

```
.agents/skills/
  my-skill/
    SKILL.md            # required – YAML frontmatter + Markdown body
    scripts/            # optional – executable helpers
    references/         # optional – longer docs loaded on demand
    assets/             # optional – templates, images, output skeletons
```

### Frontmatter

```yaml
---
name: my-skill                     # required, [a-z0-9-], <= 64 chars
description: One-paragraph routing # required, <= 1024 chars
                                   # description used by the model to decide
                                   # when to load this skill.
license: Apache-2.0                # optional
compatibility: "requires git"      # optional, free form
allowed-tools: "read_files execute_command"  # optional, advisory only in v1
disable-model-invocation: false    # optional, default false
metadata:                          # optional, free-form map
  author: your-team
  version: "1.0"
---
```

Parsing is strict (`serde(deny_unknown_fields)`). Unknown keys fail the load
and the skill is skipped with a warning. Validation rules match the spec:

- `name` and `description` are required and non-empty.
- `name` must equal the directory name and match `[a-z0-9-]+`.
- `name` <= 64 chars, `description` <= 1024 chars.
- `allowed-tools`, when present, is normalized to a whitespace-separated string
  and capped at 16 entries. **In v1 this is informational only**; tool gating
  is not enforced (see "Open Questions").

### Body

After the closing `---`, free-form Markdown. This becomes the verbatim payload
returned by the `read_skill` tool. Skills typically contain:

- A purpose / when-to-use section.
- A workflow with concrete steps.
- A "Resources" section pointing at `scripts/`, `references/`, `assets/` with
  relative paths.

## 3. Discovery & Precedence

### Roots

In precedence order (highest first):

| Scope     | Path                                                        |
| --------- | ----------------------------------------------------------- |
| `Project` | `<project_root>/.agents/skills/*`                           |
| `User`    | `<config_dir>/skills/*`                                     |
| `System`  | `<config_dir>/skills/.system/*` (managed by us, see §7)     |

`<config_dir>` is resolved via the existing
`crates/code_assistant/src/config_dir.rs:18-32` logic
(`CODE_ASSISTANT_CONFIG_DIR` -> `XDG_CONFIG_HOME/code-assistant` ->
`~/.config/code-assistant`).

`<project_root>` is the agent's working directory as resolved by
`SessionConfig::effective_project_path()`
(`crates/code_assistant/src/session/mod.rs:62-66`).

We deliberately follow the **same convention as codex-rs** (`.agents/skills/`)
to maximize ecosystem reuse. We do *not* support `.code-assistant/skills/`,
`.claude/skills/`, etc., to avoid fragmentation.

### Walk

For each root, do a bounded BFS:

- Max depth: 4 (a skill directory is normally a direct child of the root, but
  we tolerate one level of nesting for organization).
- Max entries visited per root: 2000.
- For each immediate or nested directory containing `SKILL.md`, parse it. Stop
  descending into that subtree once a `SKILL.md` is found.

### Collision handling

If multiple scopes contain a skill with the same `name`, the higher-precedence
scope wins. The shadowed skill is dropped from the catalog (but recorded in a
debug log).

### Caching

A single `SkillsManager` per `Agent` owns the discovered set. It is rebuilt:

- On `Agent::init_projects` (`crates/code_assistant/src/agent/runner.rs:759-779`).
- When the active project path changes (e.g. session reload).
- On explicit invalidation (e.g. after a skill is added at runtime; see
  Phase 2 watcher).

Cache invalidation also calls
`invalidate_system_message_cache()` (`runner.rs:1247-1249`) so the next system
prompt reflects the new catalog.

## 4. System Prompt Integration

### Where

In `Agent::get_system_prompt`
(`crates/code_assistant/src/agent/runner.rs:1166-1253`),
between `# Project Information` and `# Repository Guidance`:

```rust
// after project info, before read_guidance_files
if let Some(skills_section) = self.skills_manager.render_section(&active_skills) {
    prompt.push_str("\n\n");
    prompt.push_str(&skills_section);
}
```

`active_skills` is the set of skills currently activated in this session
(see §6).

### What

The block is plain Markdown so it works identically under all tool syntaxes:

```markdown
# Available Skills

The following skills are available. Each entry shows the skill's name, scope,
and a short description. To use a skill, call the `read_skill` tool with the
skill's name; this loads the skill's `SKILL.md` body into the conversation.
A skill may include scripts under `scripts/` (run via `execute_command`) and
references under `references/` (read via `read_files`).

Use a skill only when the user's task clearly matches its description. Do not
load multiple skills speculatively.

Active skills (already loaded; do not call read_skill again):
- pdf-extraction

Available skills:
- migration-runbook (project): Step-by-step DB migration playbook for our setup.
- skill-creator (system): Scaffold and validate new skills.
- security-review (user): Targeted review for auth, secrets, and crypto changes.
```

Implementation rules:

- Soft cap of 20 skills shown. Overflow appended as `+N more available – use
  list_skills`.
- Active skills are listed first under a separate header so the model knows
  not to call `read_skill` again for them.
- Per-skill line is `- {name} ({scope}): {description}`. We do not include
  paths to keep the catalog tight.

## 5. Tools

Two new tools are added in v1, registered in
`crates/code_assistant/src/tools/core/registry.rs:84-114`:

### `read_skill`

```rust
struct ReadSkillInput {
    name: String,
}
```

Behavior:

1. Look up the skill via `SkillsManager::find(&name)`. Resolution honors scope
   precedence.
2. Read `SKILL.md` from disk. Cap at e.g. 64 KB; truncate politely beyond
   that, mirroring the AGENTS.md loader at
   `crates/code_assistant/src/agent/runner.rs:1287-1325`.
3. Mark the skill as **active** in the session (see §6).
4. Return the body as a `ToolResult` with a small header describing scope
   and resources:

   ```
   # Skill: pdf-extraction (project)

   Resources available under <absolute-path>:
   - scripts/extract.py
   - references/api-quirks.md

   ---

   <verbatim SKILL.md body>
   ```

Permissions: `ToolPolicy::Allow`. The tool only reads from the discovered
skills directories. A path-traversal guard rejects any attempt to load
something outside a known skill root.

Scopes: registered for `Agent`, `AgentWithDiffBlocks`. **Not** registered for
sub-agent scopes by default; sub-agents inherit the parent's already-loaded
context but should not autonomously load new skills (revisitable later).

### `list_skills`

```rust
struct ListSkillsInput {
    query: Option<String>,   // optional substring filter on name + description
}
```

Returns the full catalog. Useful when the system-prompt overflow message
prompts the model to look for a skill that wasn't shown. Output mirrors the
catalog format in §4.

### Future tool: `read_skill_resource` (Phase 2)

Reads a specific file under `scripts/`, `references/`, or `assets/` of a
skill, with a path-traversal guard analogous to
`vtcode-core/src/tools/skills/mod.rs:373-415`. Until then, the regular
`read_files` tool is sufficient because absolute paths to resources are
included in the `read_skill` output header.

## 6. Session State & Stickiness

### What "active" means

A skill becomes active when `read_skill` returns successfully. Activation
implies:

1. The body is now part of the conversation history (as a tool result), so
   the LLM sees it in subsequent turns.
2. The skill is listed under "Active skills" in the system-prompt catalog so
   the model doesn't redundantly call `read_skill` again.
3. The activation persists across session reload because the tool result is
   serialized as part of the conversation.

### Why we still need explicit tracking

Two failure modes break the "tool-result is enough" assumption:

- **Context compaction.** The codebase already has a compaction pipeline
  (see `docs/context-compaction.md`). When a long session is compacted, tool
  results may be summarized or dropped. A skill body silently lost at
  compaction time would leave the model believing the skill is loaded while
  the body is gone.
- **System-prompt parity.** The catalog needs to know which skills are active
  to render the "Active skills" section, and that information must survive
  reload independently of the history.

### Implementation

Extend `SessionState` in `crates/code_assistant/src/session/mod.rs:69-99`:

```rust
pub struct SessionState {
    // ... existing fields ...
    /// Names of skills activated in this session, in activation order.
    pub active_skills: Vec<String>,
}
```

- `read_skill` appends `name` to `active_skills` (deduped).
- The system prompt renderer reads `active_skills` to populate the "Active
  skills" subsection.
- **Compaction integration:** the compactor must preserve, at minimum, the
  *most recent* `read_skill` tool result for each name in `active_skills`.
  If compaction would drop one, it must instead replace the dropped result
  with a synthetic `read_skill` result generated by re-reading the file. This
  lives in the compaction module – tracked as a small follow-up to the
  initial implementation.
- New skills directory or skill content during a session: invalidating the
  system-prompt cache is sufficient; existing active-skill bodies stay in
  history.

### Cross-session behavior

Activation is per-session by design. Starting a new session begins with an
empty `active_skills`. Sticky-across-sessions semantics could be added later
via project-level config (`<project_root>/.agents/skills.toml` with an
`auto_activate` list), but is out of scope for v1.

## 7. Bundled Skills

A small initial set is shipped embedded in the binary, mirroring the
`codex-rs` and `vtcode` approach.

### Mechanism

- Source lives under
  `crates/code_assistant/resources/skills/samples/`.
- Embedded at compile time with `include_dir!` in
  `crates/code_assistant/src/skills/bundled.rs`.
- Extracted on agent startup to `<config_dir>/skills/.system/` by an
  `install_system_skills()` helper.
- A SHA-256 fingerprint of the embedded tree is written to
  `<config_dir>/skills/.system/.fingerprint`. If unchanged, no re-extraction.
  Bumping a "salt" constant in code forces re-extraction (useful when sample
  content changes).

### Initial skill: `skill-creator`

A meta-skill that helps the agent author new skills. Body covers:

- Where to put the new skill (project vs. user scope).
- Strict frontmatter rules and the validator's expectations.
- Recommended structure (`scripts/`, `references/`, `assets/`).
- Quality checklist (description routing signal, single responsibility,
  progressive disclosure).
- A template `SKILL.md`.

We deliberately ship only one bundled skill in v1 to keep the surface
minimal and let the ecosystem (and us) grow it organically.

## 8. Configuration

### Config file

New optional file at `<config_dir>/skills.json`:

```json
{
  "enabled": true,
  "bundled_skills_enabled": true,
  "disabled": ["legacy-skill", "/abs/path/to/SKILL.md"]
}
```

- `enabled` (default `true`): master switch. When false, the section is not
  rendered and the tools return an explanatory error.
- `bundled_skills_enabled` (default `true`): when false, do not extract
  system skills, and remove any previously-extracted ones.
- `disabled`: list of skill names or absolute `SKILL.md` paths. Disabled
  skills are filtered out before catalog rendering and tool resolution.

### CLI

- `--skills-dir <PATH>`: override the user skills root for this invocation
  (analogous to `--config-dir`). Useful for development.

### Tool config

No new entries in `tools.json`. The `read_skill` and `list_skills` tools are
always available when skills are enabled.

## 9. UI Integration

The user explicitly asked to focus the UI on **input-area** activation rather
than a permanent sidebar entry. Skills are loaded in two ways from the user's
perspective:

1. **Implicitly by the model**, in response to a task that matches a skill's
   description. The catalog in the system prompt drives this; no UI needed.
2. **Explicitly by the user**, via the input area of an active session.

### Input-area UX (Phase 2/3)

Two complementary mechanisms:

- **`@skill` mention.** Typing `@` in the input area opens an autocomplete
  popover (similar to existing `@`-mentions for files, if any) listing
  available skills with their descriptions. Selecting one inserts a chip
  like `[skill: pdf-extraction]` into the message. On send, the chip is
  translated into a synthetic `read_skill` call **before** the LLM is
  invoked, so the skill body is present from the very first turn that uses
  it. Active skills are visually marked in the popover.
- **Slash command (optional).** `/skill <name>` typed at the start of a
  message activates that skill (same translation step). Convenient when the
  user knows the name and doesn't want to navigate the popover.

Both paths funnel through the same activation entry point as the model-side
`read_skill` tool, so session state stays consistent.

### Visual indication of active skills

When at least one skill is active, show a small badge near the input area
listing active skill names. Clicking a badge could offer a "remove from
context" action in a later iteration, but v1 simply shows what is active.

### Sidebar (deferred)

A sidebar section listing all available skills with enable/disable toggles
is feasible (the existing `BackendEvent` / `BackendResponse` channels would
carry it), but is intentionally postponed until the input-area flow proves
out. Tracked as future scope.

## 10. Future Scope

These items are intentionally out of v1 but are worth noting up-front so the
v1 design doesn't paint us into a corner:

- **MCP exposure of skills.** `code-assistant` already runs as an MCP server
  (`crates/code_assistant/src/mcp/`). The discovered skill catalog is a
  natural candidate to expose to MCP clients (e.g. Claude Desktop) – either
  as a list resource or as an MCP tool that returns skill bodies. Would let
  Claude Desktop benefit from the same skills repo without duplicating it.
- **Skill-declared MCP server requirements.** codex-rs allows a skill to
  declare an MCP server that should be installed when the skill is loaded
  (`agents/openai.yaml -> dependencies.tools`). Useful but adds a lot of
  surface area; deferred.
- **ENV-var dependencies.** Same source: prompt the user for missing env
  vars when a skill that requires them is activated.
- **Filesystem watcher for live reload.** Mirrors `codex-rs/core/src/skills_watcher.rs`.
  Calls `SkillsManager::reload()` and `invalidate_system_message_cache()`
  when files under any skills root change.
- **Per-project sticky activation** via a project-level `auto_activate`
  list.
- **Sub-agent access** to `read_skill`. Currently scoped out; sub-agents
  inherit context but cannot autonomously load new skills.
- **`allowed-tools` enforcement.** v1 treats this field as informational. A
  later version could constrain the tool set available during the turns
  immediately following activation.
- **Remote skills catalog.** A registry of vetted skills hosted off-repo
  with signed manifests, akin to codex-rs `hazelnuts`.

## 11. Implementation Phases

### Phase 1 – Core (1–2 days)

- New module tree:
  ```
  crates/code_assistant/src/skills/
    mod.rs
    manifest.rs        // SkillFrontmatter, parser, validation
    loader.rs          // multi-scope discovery
    manager.rs         // SkillsManager owned by Agent
    render.rs          // catalog rendering
  ```
- `SkillsManager` field on `Agent` (next to `file_trees`,
  `available_projects` at `crates/code_assistant/src/agent/runner.rs:97-100`),
  initialized in `init_projects`.
- System-prompt section injected in `get_system_prompt`
  (`runner.rs:1166-1253`).
- `read_skill` tool implementation under
  `crates/code_assistant/src/tools/impls/read_skill.rs`, registered in
  `crates/code_assistant/src/tools/core/registry.rs:84-114`.
- `active_skills: Vec<String>` added to `SessionState`
  (`crates/code_assistant/src/session/mod.rs:69-99`).
- Unit tests + integration tests modeled on
  `crates/code_assistant/src/agent/runner.rs:2629-2660` (the AGENTS.md
  precedence tests).

### Phase 2 – Polish

- `list_skills` tool.
- `<config_dir>/skills.json` (enabled / bundled toggle / disabled list).
- `--skills-dir` CLI flag.
- Bundled skills via `include_dir!` and `install_system_skills` (initially
  just `skill-creator`).
- Compaction integration: preserve or refresh the most recent `read_skill`
  result per active skill name.
- Filesystem watcher for live reload.

### Phase 3 – UI

- `@skill` mention popover in the input area.
- `/skill <name>` slash command.
- Active-skills badge near the input area.
- (Optional) `read_skill_resource` tool with path-traversal guard.

## 12. Testing

### Unit tests

- **Manifest parsing.** Valid frontmatter, unknown-key rejection, missing
  name / description, oversize fields, malformed YAML, body extraction.
- **Discovery and precedence.** Same skill name in project / user / system;
  highest scope wins. Disabled list filters out skills correctly.
- **Renderer.** Empty catalog, with and without active skills, overflow
  truncation, scope formatting.
- **Path-traversal guards** in `read_skill` (and `read_skill_resource` once
  added).

### Integration tests

- A fixture project under `crates/code_assistant/tests/` that contains a
  `.agents/skills/sample/SKILL.md`. The agent loads it into the catalog,
  calls `read_skill("sample")`, and the body appears verbatim in the next
  prompt.
- Round-trip through `SessionState` persistence: activate a skill, save the
  session, reload, confirm `active_skills` is preserved and the catalog
  renders the active marker.
- Compaction: long session with one active skill, force compaction, confirm
  the body is still present (or refreshed) afterwards.
- Tool-mode parity: run the same scenario under `Native`, `Xml`, `Caret`
  and confirm catalog appearance and tool dispatch behave identically.

### Recording / playback

The recording subsystem stores LLM requests verbatim. A canned recording for
a skills test should treat `read_skill` results like any other tool result;
no special handling is needed. We do, however, need a deterministic skill
body for recordings, so test fixtures must ship with stable file contents.

## 13. Open Questions

1. **`allowed-tools` enforcement.** v1 ignores this field. Should a future
   version (a) expose only the listed tools to the model during turns
   following activation, (b) emit a warning if the model uses a non-listed
   tool, or (c) keep it informational forever? Codex and vtcode disagree
   here; vtcode enforces, codex documents but does not enforce.
2. **Multiple active skills and ordering.** If three skills are active and
   their guidance conflicts, what wins? The simplest answer is "model decides
   from full context"; we should at least add a brief note to the catalog
   header in that case.
3. **Sub-agent access.** Should a sub-agent inherit the parent's active
   skills automatically (yes, by virtue of inheriting the prompt), and
   should it be allowed to call `read_skill` itself (currently no)?
