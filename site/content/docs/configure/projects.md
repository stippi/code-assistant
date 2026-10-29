---
title: Projects and format-on-save
linkTitle: Projects
weight: 20
lede: Projects are the folders the agent works in. Each one can run your formatter after the agent edits a file, without confusing the model.
frontends: [app, tui, acp]
---

## `projects.json`

```json
{
  "api-client": {
    "path": "/Users/me/work/api-client",
    "format_on_save": { "**/*.rs": "cargo fmt" }
  },
  "web-shop": {
    "path": "/Users/me/work/web-shop",
    "format_on_save": {
      "**/*.ts": "prettier --write {path}",
      "**/*.tsx": "prettier --write {path}",
      "**/*.css": "prettier --write {path}"
    }
  }
}
```

- The **key** is the project name shown in the sidebar and used by the tools.
- The agent can read every configured project, but each session belongs to
  one. Its edits and commands run there.
- Starting the terminal in a folder that isn't listed creates a temporary
  project. In the app, a project folder's hover action saves it here.
- A session stays tied to the project it was created in.

## Format-on-save {#format-on-save}

`format_on_save` maps glob patterns to shell commands. After the agent writes
or edits a matching file, the command runs. Use `{path}` for formatters that
take a file (`prettier --write {path}`). Leave it out for whole-project
formatters (`cargo fmt`).

### Why it's more than running a formatter

When a formatter rewrites a file right after the agent edited it, the model's
picture of the file is out of date. Its next edit searches for text that no
longer exists and fails, often for reasons that aren't obvious.

Code Assistant fixes this. After formatting, it works out what the agent's
edit looks like in formatted form and **rewrites the tool call in the history
to match**, as if the model had written formatted code from the start. The
model's view stays correct, and it doesn't need to spend tokens reading the
file again. If the edit can't be matched reliably, the file is still formatted
but the history is left as it was.

## Project guidance

Put an `AGENTS.md` (or `CLAUDE.md`) in the project root for the agent to read
in every session. See [Skills and AGENTS.md](/docs/configure/skills/).
