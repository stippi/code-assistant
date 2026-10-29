---
title: Skills and AGENTS.md
weight: 50
lede: AGENTS.md holds what the agent should always know about a repository. Skills are playbooks it loads only when a task needs them.
frontends: [app, tui, acp]
---

## AGENTS.md: project guidance

Put an `AGENTS.md` in the project root and it is added to the system prompt of
every session in that project. If there is none, a `CLAUDE.md` is used. Matching
is case-insensitive. A global `AGENTS.md` in the config folder is added
first, so the project's file can refine it.

Good content: how to build and test, code conventions, what not to touch,
where the important entry points are.

```markdown
# Repository Guidance

## Commands
- `cargo test --workspace` runs all tests
- `cargo clippy --all-targets -- -D warnings` must pass before a commit

## Conventions
- Errors use `anyhow` in binaries, `thiserror` in libraries
- New UI code goes in `crates/ui/src/components/`
```

## Skills

A skill is a folder whose name matches the skill's name, with a `SKILL.md`
inside:

```text
.agents/skills/
  release-notes/
    SKILL.md        # required: front matter + instructions
    scripts/        # optional: helper scripts
    references/     # optional: longer docs, read on demand
    assets/         # optional: templates
```

```markdown
---
name: release-notes
description: Write release notes from the git history since the last tag. Use when asked for release notes or a changelog entry.
---

# Release notes

1. Find the last tag with `git describe --tags --abbrev=0`.
2. Group the commits since then into Features, Fixes and Internal.
3. ...
```

`name` (lowercase letters, digits, `-`) and `description` are required. The
description tells the model *when* to use the skill. Only the name and
description are always in the system prompt. The body is loaded when the agent
calls `read_skill`, or when you invoke the skill with `/release-notes …`.

### Where skills are found

In order of precedence:

| Scope | Folder |
|---|---|
| Project | `<project>/.agents/skills/` |
| User | `<config folder>/skills/` |
| Bundled | shipped with Code Assistant (for example `skill-creator`) |

### Switch skills off

In **Settings → Skills**: **Enable skills** turns the feature off, **Use
bundled skills** hides the bundled ones, and each skill has its own switch.
The settings are stored in `skills.json`:

```json
{
  "enabled": true,
  "bundled_skills_enabled": true,
  "disabled": ["legacy-skill"]
}
```
