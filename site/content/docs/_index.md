---
title: Guides
lede: Everything about Code Assistant, ordered by what you want to do. Each guide shows the steps for the app, the terminal and editors.
---

Code Assistant is an AI coding agent. You give it a task in plain language. It
then reads and searches your code, edits files and runs commands, one tool call
at a time, until the task is done or it needs you. Unlike most agents, it
shows you each step in full: which tool ran, with which input, and what came
back to the model.

## One agent, several frontends

The same agent runs behind four frontends. They share the configuration and the
session store, so a session started in one can be continued in another.

| Frontend | Start it with | Best for |
|---|---|---|
| **App** (native GUI) | `code-assistant` | Day-to-day work: inbox, review panel, live browser, voice |
| **Terminal** (TUI) | `code-assistant --tui` | Working over SSH or staying in the shell |
| **Editor** (ACP) | `code-assistant acp` | Using it inside Zed or another ACP client |
| **MCP server** | `code-assistant server` | Giving its tools to Claude Desktop and other MCP clients |

{{< callout tip "Pick your frontend once" >}}
Use the **App / Terminal / Editor** switch at the top of the page. Every guide
then shows the steps for your frontend, and your choice is remembered. Pages
marked <span class="only">app</span> are about features only the app has.
{{< /callout >}}

## Where to start

- New here? [Install it](/docs/start/install/), [connect a model](/docs/start/first-run/) and [run a first task](/docs/start/first-task/).
- Know the basics? Look at the [tour of the window](/docs/work/window/), [permissions](/docs/work/permissions/) and [sub-agents](/docs/long/sub-agents/).
- Setting it up for a team? See [projects](/docs/configure/projects/), [MCP servers](/docs/configure/mcp/) and [skills and AGENTS.md](/docs/configure/skills/).
