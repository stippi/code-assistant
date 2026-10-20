# Architecture panel

Status: prototype (2026-10-20). Code: `code_assistant_core::architecture`,
`SessionService::architecture_overview` / `architecture_summary`,
`ui_gpui::main_screen::right_panel::architecture`, *Settings → Architecture*.

## Goal

Working with a coding agent, the developer reads less code and needs a
higher-level picture of what is there: which components exist, what each is
responsible for, what it exposes, which rules it follows and how the parts
depend on each other. The *Architecture* view of the right panel (next to
Review, Files and Browser) shows that picture and keeps it current while the
agent changes the project.

## What it shows

One level of the project at a time:

- **The shown component** (the project first): a one-sentence summary, its
  responsibility, **constraints** (layering and dependency direction, what it
  must not depend on, state ownership, extension points), key interfaces,
  and which sibling components it uses / is used by.
  Summary and constraints show by default; *More* adds the rest.
- **Its parts as boxes**: name, kind (package, module, file) and a one-line
  responsibility. Arrows go from a part to the parts it uses; the layout puts
  users above what they use, so arrows point down and the foundation sits at
  the bottom. Parts that depend on each other in a cycle share a row (ranks
  are computed on strongly connected components). Only the transitive
  reduction is drawn (`a → c` is left out when `a → b → c` exists); hovering
  a box adds all of its direct edges. Parts without any dependency on this
  level sit in rows of their own. Test code is drawn dashed and nothing
  points at it.
- **Hover** highlights a box's dependencies (blue: what it uses, green: what
  uses it) and shows its details below the graph: responsibility,
  constraints, uses / used by, and its public items.
- **Click** a box to open its level (breadcrumb to go back up); a file box
  opens the file in the Files view.

## Comments

Components take comments like file lines and chat passages do. The comment
button on a box (shown on hover) or on the shown component's card opens the
usual comment card; the comment joins the composer's draft (one per
component; the button edits it again). Commented boxes are outlined and
carry the comment mark, the card and the inspector show the text, and the
composer chip reveals it in the Architecture view.

A component comment is a `LineComment` with `on_component` set: `file` is
the component's folder or file (the project root for the project), and the
excerpt is what the overview said about it (name, kind, summary,
constraints, uses / used by, parts), so the agent sees what the user looked
at. On send it renders as

```xml
<comment on="component" path="crates/llm">
<component>
llm (package) at crates/llm
…
</component>
split providers out
</comment>
```

inside the usual `<line-comments>` block.

## How it is built: deterministic structure, small model calls

The token cost stays low by splitting the work:

1. **Scan, no model** (`architecture::scan`, `architecture::extract`).
   The project's files (gitignore respected, build output folders skipped)
   become a component tree:
   - packages from manifests (`Cargo.toml` with `[package]`, `package.json`,
     `pyproject.toml`/`setup.py`) with their description and declared
     dependencies, README paragraphs as fallback descriptions;
   - folders as modules, files as leaves. A package's `src` folder and a
     folder that only groups packages (`crates/`, `packages/`) are
     transparent; folder chains without own files collapse
     (`src/main/java/com/acme`); entry files (`lib.rs`, `mod.rs`,
     `index.ts`, `__init__.py`) and `foo.rs` next to `foo/` belong to the
     folder.
   - per file: its doc comment, its public items as one-line signatures
     and its imports, by line-oriented heuristics for Rust (`pub` items,
     `use` trees, `crate::`/`super::`/crate-name paths), TypeScript/JS
     (`export`s, `import`/`require`, relative paths, workspace packages,
     `@/` aliases) and Python (top-level `def`/`class`, absolute and
     relative imports). Other languages are counted but not read.
   - imports are resolved to project paths; an edge `a → b` is recorded on
     the level of the lowest common ancestor of the two files' components.

   A rescan only re-reads files whose size or modification time changed
   (this repository: ~0.6 s for the first scan, ~50 ms after).

2. **Explain, one model call per component** (`architecture::digest`,
   `architecture::analyzer`). The model gets a *digest* of the component —
   its description, interface and dependencies, then per part its doc,
   exposed items and which siblings it uses (≤ ~16k characters) — and
   answers JSON: summary, responsibility, constraints, interfaces and a
   one-liner per part. One call thus explains a component *and* labels all
   its boxes.

   The digest contains no function bodies, so most edits leave it
   unchanged. Its hash is the cache key: summaries are stored per project
   in `<data_dir>/architecture/<hash of root>.json`, a few per component,
   so a branch switch or revert back to an earlier shape finds its
   summaries again (that is what "caching by commit" amounts to, without
   depending on git). Identical requests in flight are made once; at most
   three calls run at a time.

   Calls happen only for what is viewed: the project when the panel opens,
   a component when its level is opened. A changed component keeps showing
   its previous explanation (marked as changed) while the new one is made.

3. **Updates.** While the panel is open, a `TreeWatcher` on the project
   root rescans after relevant files changed (source files, manifests,
   READMEs; uncommitted edits included), and the shown level is explained
   again if its digest changed. The refresh button rescans and forces a new
   explanation of the shown level.

## Settings (`<config_dir>/architecture.json`, *Settings → Architecture*)

```json
{
  "model": "",            // a models.json key; empty = the default model
  "auto_summarize": true, // explain levels when shown; off = Explain button
  "watch": true           // rescan on file changes while the panel is open
}
```

A fast, inexpensive model is enough; it never sees whole files.

## Seams and tests

- `architecture::Summarizer` is the model seam; `LlmSummarizer` builds the
  client from `models.json` (or the runtime's `llm_client_factory`).
  `SessionService::with_architecture` injects an analyzer (the GPUI
  `TestCore` uses a fixed model and no disk cache).
- Unit tests: extractors per language, component tree rules and edges
  (Cargo workspace, npm workspace with aliases, Python src layout), digest
  parsing, cache behaviour (body edits keep the summary, interface changes
  make it stale, earlier states are found again, restart survives).
- `ui_gpui` tests: layout (ranks, wrapping, cycles, barycenter) and an
  end-to-end test opening the panel on a real project with a scripted model,
  drilling into a crate and opening a file.

## Follow-ups

- Persist the shown level per session (today: per view lifetime).
- Better extractors (tree-sitter is already a dependency of the GUI) and
  more languages (Go, Java/Kotlin).
- Group large flat levels (e.g. 60+ files) into clusters before showing.
- Let the agent read the overview (a tool or a system-prompt section), so
  the same picture guides the agent.
- Edge labels (which items are used); edge routing around boxes.
