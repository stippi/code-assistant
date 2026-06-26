---
name: skill-creator
description: Scaffold, author, and validate an Agent Skill. Use when the user wants to create a new skill, turn a repeatable workflow into a reusable SKILL.md, or fix a skill that fails to load.
---

# Skill Creator

A *skill* is a reusable, task-specific playbook stored as a `SKILL.md` file in a
directory named after the skill. Skills are advertised in the system prompt by
name + description (progressive disclosure); their full instructions are loaded
on demand with the `read_skill` tool.

Use this skill whenever the user asks to create, author, or improve a skill.

## 1. Choose a scope

A skill lives in one of three scopes. Pick based on how widely it should apply:

- **project** — `<project_root>/.agents/skills/<name>/` — specific to one repo.
  Load with `read_skill(project="<project-name>", name="<name>")`; read its
  resources with `read_files(project="<project-name>", ...)`.
- **user** — addressed by the scope token `:config:` — your personal skills,
  available in every project. Load with `read_skill(project=":config:", ...)`.
- **system** — token `:system:` — bundled skills (like this one). Usually
  read-only.

On a name collision, project wins over user wins over system.

## 2. Write `SKILL.md`

The file MUST start with a YAML frontmatter block delimited by `---` lines,
followed by a free-form Markdown body:

```
---
name: my-skill
description: One-paragraph routing description the model uses to decide when to load this skill.
---

# My Skill

...instructions...
```

Frontmatter rules (enforced by the loader):

- `name` is required, must be **lowercase letters, digits, and hyphens** only,
  at most 64 characters, and must **equal the directory name**.
- `description` is required, at most 1024 characters. Write it as a routing
  signal: say *when* to use the skill, not just what it does.
- Other keys (license, etc.) are allowed and ignored for now.

## 3. Add resources (optional)

Bundle supporting files under the skill directory and reference them from the
body with paths relative to the skill directory:

- `scripts/` — runnable helpers. Prefer running them with `execute_command`
  rather than reading them into context.
- `references/` — longer docs to read on demand with `read_files`.
- `assets/` — templates or output skeletons.

Keep `SKILL.md` itself short: it should orchestrate, pointing at the right
resource for each sub-task instead of inlining everything.

## 4. Validate

- The directory name equals `name`.
- Frontmatter parses and `name`/`description` satisfy the rules above.
- Referenced resource paths exist and are relative to the skill directory.
- Load it once with `read_skill` to confirm the body renders as expected.

## Template

```
---
name: <kebab-case-name>
description: Use this skill when <trigger>. It <what it does>.
---

# <Human Title>

## When to use
<one or two sentences>

## Workflow
1. <step>
2. <step>

## Resources
- `references/<doc>.md` — <what it contains>
- `scripts/<tool>` — <what it does> (run with execute_command)
```

## Quality checklist

- Single responsibility: one skill, one job.
- The description is a strong routing signal (the model only sees this until it
  loads the skill).
- Progressive disclosure: short body, details in `references/`.
- Deterministic work lives in `scripts/`, not in prose.
