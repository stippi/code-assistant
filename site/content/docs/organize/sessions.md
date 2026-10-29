---
title: Projects, sessions and the inbox
linkTitle: Sessions and the inbox
weight: 10
lede: Every conversation is a session in a project. The app's sidebar works like an inbox. It shows what needs you first and lets finished work settle out of the way.
frontends: [app, tui, acp]
---

## Sessions

A **session** is one conversation with its history, settings (model, effort,
permissions, sandbox, tools) and state. It belongs to the project it was
created in. Sessions are saved as you go and survive restarts. They are
stored in the data folder (see [Configure](/docs/configure/)).

The agent gives each session a short title once it knows what the task is.

{{< frontends >}}
{{< fe "app" >}}
Click **New session** in the sidebar, or use a project folder's hover action
to start one in that project. Click a session to open it. Hover it to
**Settle** or **Delete** it.
{{< /fe >}}
{{< fe "tui" >}}
`/sessions` lists the sessions of the current project and switches to the one
you pick. `--continue-task` starts with the most recent session instead of a
new one.
{{< /fe >}}
{{< fe "acp" >}}
Your editor lists previous Code Assistant threads and can load them. Code
Assistant sends a title for each session.
{{< /fe >}}
{{< /frontends >}}

## The inbox {#inbox}

{{< fe "app" >}}
The sidebar's **Active** view lists every session that isn't settled, from all
projects, newest first. A row shows what the session is doing:

| Label | Meaning |
|---|---|
| *Needs approval* | A tool call waits for your permission |
| *Needs answer* | The agent asked you questions |
| *Failed* | The last run ended with an error |
| *Working*, *Rate limited*, *Retrying* | The agent is busy (row shown quieter) |
| *Monitoring* | Idle, but a wakeup or monitor will continue it |
| *Running elsewhere* | Another Code Assistant process runs this session |
| *Unread* (dot) | Finished with an answer you haven't seen |

When a session works on a branch, the row shows the branch. If the branch has a
pull request, it also shows the number, the review state and the checks
(read with the GitHub CLI, `gh`).
{{< /fe >}}
{{< fe "tui" >}}The inbox is part of the app. The terminal shows the current session only.{{< /fe >}}
{{< fe "acp" >}}The inbox is part of the app. Your editor shows its own thread list.{{< /fe >}}

## Settling {#settle}

Finished work leaves the inbox by **settling**. Nothing is deleted. A settled
session stays in its project folder and can be brought back with **Un-settle**.
Sessions settle:

- **by hand**, with the row's **Settle** action,
- **after some days without activity** (default 14, set in
  **Settings → General → Settled sessions**, `0` turns it off),
- **when their branch is merged**, locally or through its pull request
  (switch in **Settings → General**). Squash and rebase merges count.

A busy session, or one with a pending wakeup, never settles on its own.

## Projects view

**Projects** shows one folder per project with all its sessions, settled ones
shown quieter, five at a time (**Show more** for the rest). Drag folders into
the order you like. A collapsed folder shows a dot for the most urgent thing
inside: an approval, a failure or an unread session.

## Projects

A project is a named folder in `projects.json`. Add one with **New project**
in the sidebar, or see [Projects and format-on-save](/docs/configure/projects/).
A folder you start the terminal in without configuring it becomes a
**temporary project**. Save it permanently with the folder's hover action in
the app.
