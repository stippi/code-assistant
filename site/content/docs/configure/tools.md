---
title: Built-in tool groups
linkTitle: Built-in tools
weight: 40
lede: Turn groups of built-in tools on or off, globally, per project and per session, the same way as MCP servers.
frontends: [app, tui, acp]
---

## The groups

| Group | Tools |
|---|---|
| **Browser** | every `browser_*` tool ([details](/docs/work/browser/)) |
| **Web** | `web_search`, `web_fetch`, `perplexity_ask` |
| **Terminal** | `execute_command`, `write_stdin` |
| **Sub-agents** | `spawn_agent` |
| **Wakeups** | `schedule_wakeup`, `cancel_wakeup` |
| **Monitors** | `monitor`, `stop_monitor` |
| **Session history** | `search_sessions`, `get_session_content` |

The file tools (list, read, search, write, edit, delete) are always on.

## Three levels

1. **Global**, in **Settings → Tools** or `tools.json`. A group can be
   switched off completely, or set to start off in new sessions.
2. **Project**. When you switch a group in a session, the project remembers it,
   and its next sessions start the same way.
3. **Session**, in the selector row's **Tools** menu. The change applies from
   the next agent run.

```json
{
  "builtin_tools": {
    "browser": { "off_in_new_sessions": true },
    "terminal": { "enabled": false }
  }
}
```

## Web search

`web_search` uses DuckDuckGo and needs no key. `web_fetch` loads a page and
returns it as text. For AI-powered search, add a Perplexity key to
`tools.json`, and the `perplexity_ask` tool appears:

```json
{ "perplexity_api_key": "${PERPLEXITY_API_KEY}" }
```

Tools that are missing their configuration are not offered to the agent.
