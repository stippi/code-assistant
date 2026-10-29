---
title: Pick a frontend
weight: 40
lede: The app, the terminal, your editor, or another MCP client. Here is what each one can do.
frontends: [app, tui, acp]
---

All frontends run the same agent with the same tools and configuration. They
differ in what they show you and how you control it.

## The app

```bash
code-assistant
```

The native app is built on [GPUI](https://www.gpui.rs/), the UI framework of
the Zed editor. It has the most features:

- a sidebar **inbox** that shows which sessions need you, across all projects
- full **tool cards** with diffs, terminal output and sub-agent transcripts
- a **Review** panel with the working tree diff and line comments
- a **Files** panel with rendered Markdown and SVG
- the agent's **browser**, live, with a login handoff
- **worktrees** per session, **questions** the agent asks as forms, and **voice mode**

Start here with the [tour of the window](/docs/work/window/).

## The terminal

```bash
code-assistant --tui
```

A full-screen terminal UI with a transcript, slash commands with
autocompletion, permission prompts, a plan view and sub-agent controls. Good
over SSH and for quick tasks. See [Terminal (TUI)](/docs/frontends/terminal/).

## Your editor (ACP)

```bash
code-assistant acp
```

An [Agent Client Protocol](https://agentclientprotocol.com) agent. Editors
like [Zed](https://zed.dev) show it in their agent panel. The agent reads and
writes files through the editor, so unsaved changes are visible to it, and
runs commands in the editor's terminals. See [Editors via ACP](/docs/frontends/acp/).

## MCP server

```bash
code-assistant server
```

A headless [Model Context Protocol](https://modelcontextprotocol.io) server.
Clients like Claude Desktop get Code Assistant's file, search and command
tools for your configured projects. There is no agent loop of its own here.
The client's model drives the tools. See [MCP server mode](/docs/frontends/mcp-server/).

## What works where

| Feature | App | Terminal | Editor (ACP) |
|---|---|---|---|
| Model and effort per session | ✓ | ✓ `/model`, `/effort` | ✓ config options |
| Permission tiers | ✓ | ✓ `/permissions` | ✓ session modes |
| Sandbox | ✓ per session | flag | flag |
| Skills and slash commands | ✓ | ✓ | ✓ |
| Goals (`/goal`) | ✓ | ✓ | ✓ |
| Sub-agents | ✓ panel | ✓ `/agents` | ✓ |
| `/new`, `/handoff` | ✓ | ✓ | (none) |
| Plan | ✓ | ✓ `/plan` | ✓ |
| Review panel, line comments | ✓ | (none) | your editor |
| Questions as forms (`ask_question`) | ✓ | (none) | (none) |
| Live browser panel | ✓ | (none) | (none) |
| Worktrees, inbox, voice | ✓ | (none) | (none) |
