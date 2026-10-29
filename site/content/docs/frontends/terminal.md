---
title: Terminal (TUI)
weight: 10
lede: A full-screen terminal interface with the same agent, sessions and configuration as the app. Good over SSH and for quick tasks.
frontends: [tui]
---

```bash
cd ~/work/my-app
code-assistant --tui
code-assistant --tui --model anthropic-main/claude-sonnet-5-5 --task "Explain this codebase"
```

The folder you start in is the session's project. Run `code-assistant --help`
for all options, or see [CLI options](/docs/reference/cli/).

## The screen

From top to bottom: the **transcript** (your messages, the agent's answers and
tool calls), the status line with running **sub-agents** and pending
requests, and the **composer**. The plan panel is toggled with `/plan`.

## Keys

| Keys | Action |
|---|---|
| <kbd>Enter</kbd> | Send (queued while the agent works) |
| <kbd>Shift</kbd>+<kbd>Enter</kbd> | New line |
| <kbd>Esc</kbd> | Close a popup or message. Otherwise stop the agent |
| <kbd>Ctrl</kbd>+<kbd>C</kbd> | Quit |
| <kbd>Ctrl</kbd>/<kbd>Alt</kbd>+<kbd>V</kbd> | Paste an image from the clipboard |
| <kbd>Alt</kbd>/<kbd>Ctrl</kbd>+<kbd>←</kbd>/<kbd>→</kbd> | Move by word |
| <kbd>↑</kbd>/<kbd>↓</kbd>, <kbd>Tab</kbd> | Move through and accept slash-command suggestions |

## Commands

Type <kbd>/</kbd> for a list with autocompletion. Skills are listed too.

{{< term >}}[dim]›[/] /help{{< /term >}}

| Command | Does |
|---|---|
| `/help` (`/h`) | Show available commands |
| `/model [name]` (`/m`) | List models, or switch to one |
| `/effort [level]` (`/e`) | Show or set the reasoning effort |
| `/provider` (`/p`) | List providers |
| `/current` (`/c`) | Show the active model |
| `/permissions [tier]` | Show or set the [permission tier](/docs/work/permissions/) |
| `/allow`, `/always`, `/deny` | Answer the pending permission request |
| `/plan` | Toggle the plan panel |
| `/goal …` | [Set or manage a goal](/docs/long/goals/) |
| `/new [prompt]`, `/handoff [instruction]`, `/compact` | [Start a fresh context](/docs/long/context/) |
| `/clear` | Clear the conversation context |
| `/agents [open <id> \| back \| cancel <id\|all>]` | [Control sub-agents](/docs/long/sub-agents/) |
| `/sessions` (`/resume`) | Switch to another session |
| `/skill`, `/<skill-name> <request>` | [Use a skill](/docs/work/commands-skills/) |

## Permission prompts

With an asking [permission tier](/docs/work/permissions/), a prompt opens
above the composer for each request: **Allow once**, **Always allow
(session)**, **Deny**. <kbd>Esc</kbd> dismisses it, and the request stays open for
`/allow`, `/always` or `/deny`.

## Sessions are shared with the app

The terminal stores sessions where the app does. A session started in the
terminal shows up in the app's sidebar and can be continued there, with the
review panel, and the other way round. If a session is running in another
process, it is shown as *Running elsewhere* and can't be changed from here.
