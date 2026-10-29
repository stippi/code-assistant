---
title: Decide what the agent may do without asking
linkTitle: Permissions and sandbox
weight: 40
lede: By default the agent runs every tool without asking. You can have it ask first for actions that leave your machine, for anything that writes, or for every step. You can also keep its commands inside the project.
frontends: [app, tui, acp]
---

Two separate controls work together:

- The **permission tier** decides *whether* a tool call runs without asking you.
- The **sandbox** decides *what* a shell command can touch once it runs.

## Permission tiers

| Tier | The agent asks before… | In the app |
|---|---|---|
| `bypass-all` (default) | nothing | *Bypass Permissions* |
| `outward-tools` | tools whose effects leave your machine, like sending a message or calling a remote service | *Ask Before Outward Actions* |
| `write-tools` | anything that isn't read-only: edits, commands, MCP tools that aren't marked read-only | *Ask Before Writes* |
| `all-tools` | every tool call | *Ask For All Tools* |

The tier is set per session and applies from the agent's next run.

{{< step 1 "Pick a tier" >}}
{{< frontends >}}
{{< fe "app" >}}
Open the **Permissions** dropdown in the selector row.

{{< selectors on="Ask Before Writes" >}}<div class="menu"><div>Bypass Permissions<small>Never ask</small></div><div>Ask Before Outward Actions<small>Effects that leave this machine</small></div><div class="cur">Ask Before Writes ✓<small>Anything that isn't read-only</small></div><div>Ask For All Tools<small>Every tool call</small></div></div>{{< /selectors >}}
{{< /fe >}}
{{< fe "tui" >}}
Run `/permissions` to see the current tier, or give it a new one:

{{< term >}}[dim]›[/] /permissions write-tools
[dim]›[/] /permissions
[acc]Permission tier: WriteTools[/]{{< /term >}}
{{< /fe >}}
{{< fe "acp" >}}
The tiers are offered as **session modes** (`bypass-all`, `outward-tools`,
`write-tools`, `all-tools`). Pick one in your editor's mode picker. The
editor sends `session/set_mode`.
{{< /fe >}}
{{< /frontends >}}
{{< /step >}}

{{< step 2 "Answer a request" >}}
{{< frontends >}}
{{< fe "app" >}}
A banner appears above the input. The session is marked *Needs approval* in
the inbox, so you notice it even while you're looking at another session.

{{< permission-request text="<code>execute_command</code>: <code>cargo test</code>" >}}

**Always (session)** allows this tool for the rest of the session.
{{< /fe >}}
{{< fe "tui" >}}
A prompt opens above the composer. Choose with the arrow keys and
<kbd>Enter</kbd>. Requests come one at a time, oldest first.

{{< term >}}[amb]? execute_command wants to run: cargo test[/]
  [acc]› Allow once[/]
    Always allow (session)
    Deny{{< /term >}}

<kbd>Esc</kbd> closes the prompt without answering. The request stays open,
and you can answer it later with `/allow`, `/always` or `/deny`.
{{< /fe >}}
{{< fe "acp" >}}
Requests arrive as your editor's own permission prompts, with the same three
choices.
{{< /fe >}}
{{< /frontends >}}
{{< /step >}}

Requests from [sub-agents](/docs/long/sub-agents/) go through the same prompt.

{{< callout warn "MCP server mode" >}}
The headless MCP server has nobody to ask, so with a tier that asks, a gated
call is denied with an error. Keep `bypass-all` there.
{{< /callout >}}

## Command sandbox

The sandbox limits `execute_command`, the tool that runs shell commands:

| Mode | Commands can… | In the app |
|---|---|---|
| `danger-full-access` (default) | do anything your user account can | *Full Access* |
| `workspace-write` | write only inside the project. No network unless allowed | *Workspace Write* |
| `read-only` | read, but not write | *Read Only* |

{{< frontends >}}
{{< fe "app" >}}Pick it in the **Sandbox Mode** dropdown of the selector row, per session.{{< /fe >}}
{{< fe "tui" >}}
```bash
code-assistant --tui --sandbox-mode workspace-write --sandbox-network
```
`--sandbox-network` allows outbound network access in `workspace-write`.
{{< /fe >}}
{{< fe "acp" >}}
Add the flags to the agent's arguments in your editor settings:

```json
"args": ["acp", "--sandbox-mode", "workspace-write"]
```
{{< /fe >}}
{{< /frontends >}}

{{< callout tip "A common setup" >}}
`bypass-all` with `workspace-write`: the agent never stops to ask, but its
commands can't change anything outside the project.
{{< /callout >}}

## At a glance

| | App | Terminal | Editor (ACP) |
|---|---|---|---|
| Switch tier | Permissions dropdown | `/permissions <tier>` | Session mode |
| Answer a request | Banner above the input | Prompt, or `/allow` `/always` `/deny` | Editor prompt |
| Sandbox | Sandbox Mode dropdown | `--sandbox-mode` | `--sandbox-mode` in args |
