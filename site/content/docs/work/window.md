---
title: A tour of the window
linkTitle: Tour of the window
weight: 10
only: app
lede: The app window has three columns, your sessions, the conversation and a panel for reviewing, plus a title bar.
frontends: [app]
---

{{< tour >}}

## Title bar

From left to right:

- **Sidebar toggle**, **theme** (light/dark) and **zoom** (− / percentage / +).
  The theme and zoom are remembered.
- **Voice** starts and stops [voice mode](/docs/organize/voice/).
- **Review · Files · Browser** switches the right panel. Click the selected
  view again to close the panel.
- **About** and **Settings**.

## Sidebar

The sidebar has two views:

- **Inbox** lists unsettled sessions from all projects. A session that needs
  you, for example *Needs approval*, *Needs answer* or *Unread*, stands out.
  Sessions that are working stay in the list but look quieter.
- **Projects** lists every session, grouped in project folders that you can
  drag into the order you like.

See [Projects, sessions and the inbox](/docs/organize/sessions/).

## Conversation

Your messages, the agent's answers, its reasoning (collapsible) and a card for
each tool call. Below them is the **composer**: the input with its
attachments, and the **selector row** with the session's settings:

{{< selectors caption="Model · reasoning effort · permissions · sandbox · tools and MCP servers · working directory (worktree)" />}}

Above the input, panels appear when they are needed: running **sub-agents**,
a **permission request**, a **question** from the agent.

## Right panel

- **Review**: the diff of the working tree or of the branch against its base,
  with line comments. See [Review the agent's changes](/docs/work/review/).
- **Files**: the project tree and a file viewer. See [Browse project files](/docs/work/files/).
- **Browser**: the agent's browser, live. See [Test in a real browser](/docs/work/browser/).

## Keyboard

| Keys | Action |
|---|---|
| <kbd>Enter</kbd> | Send the message (queued if the agent is working) |
| <kbd>Shift</kbd>/<kbd>Alt</kbd>/<kbd>Cmd</kbd> + <kbd>Enter</kbd> | New line |
| <kbd>Esc</kbd> | Stop the agent |
| <kbd>/</kbd> at the start of the input | Slash commands and skills |
| <kbd>Cmd</kbd>+<kbd>W</kbd> / <kbd>Cmd</kbd>+<kbd>Q</kbd> | Close the window / quit |
