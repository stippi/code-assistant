---
title: Run your first task
weight: 30
lede: Add a project, start a session, and watch the agent work through a real task.
frontends: [app, tui, acp]
---

## Add a project

A **project** is a folder the agent works in. Every session belongs to one
project. The agent can read the other configured projects too, but searches,
edits and commands run in the session's project.

{{< frontends >}}
{{< fe "app" >}}
In the sidebar, click **New project**, choose the folder, and give it a name.
The project shows up in the sidebar's **Projects** view. Click
**New session** to start a conversation in it.
{{< /fe >}}
{{< fe "tui" >}}
`cd` into the project folder before you start. The folder you launch from is
the session's project. A folder that isn't configured becomes a temporary
project automatically.

```bash
cd ~/work/my-app
code-assistant --tui
```
{{< /fe >}}
{{< fe "acp" >}}
Your editor tells the agent which folder is open, so there is nothing to set up.
Open the project in Zed and start a new thread with Code Assistant in the
agent panel.
{{< /fe >}}
{{< /frontends >}}

## Give it a task

Describe the result you want, not the steps. Good first tasks are small and
easy to check:

> Explain how errors are handled in this codebase, with the main files involved.

> The `parse_config` test fails on Windows. Find out why and fix it.

Press <kbd>Enter</kbd> to send. <kbd>Shift</kbd>+<kbd>Enter</kbd> adds a new line.

In the terminal you can also pass the first task on the command line:
`code-assistant --tui --task "Explain this codebase"`.

## Watch what it does

The agent answers with text and **tool calls**: it searches files, reads them,
edits them, runs commands. Each tool call shows up as a card that you can open
to see exactly what went to the model and what came back. See
[Follow what the agent does](/docs/work/conversation/).

You don't have to wait. Type the next message while the agent works. It is
queued and the agent reads it at its next step. To stop the agent, press
<kbd>Esc</kbd>, or click **Stop** in the app.

## Guide it with your project's rules

If the project has an `AGENTS.md` (or a `CLAUDE.md`) in its root, the agent
reads it at the start of every session. That's the place for build commands,
test commands and code conventions. See [Skills and AGENTS.md](/docs/configure/skills/).
