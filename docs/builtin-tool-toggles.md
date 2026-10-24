# Built-in Tool Toggles

Groups of built-in tools can be switched on and off the same way as MCP
servers (see `mcp-client-mode.md`): globally, per project, and per session.

## Groups

Defined in `code_assistant_core::tools::builtin_selection::GROUPS`:

| id                | tools                                              |
|-------------------|----------------------------------------------------|
| `browser`         | every `browser_*` tool                             |
| `web`             | `web_search`, `web_fetch`, `perplexity_ask`        |
| `terminal`        | `execute_command`, `write_stdin`                   |
| `sub_agents`      | `spawn_agent`                                      |
| `wakeups`         | `schedule_wakeup`, `cancel_wakeup`                 |
| `monitors`        | `monitor`, `stop_monitor`                          |
| `session_history` | `search_sessions`, `get_session_content`           |

All other tools (files, plan, skills, …) are always on.

## Levels

- **Global** (`<config_dir>/tools.json`, *Settings → Tools*):

  ```json
  { "builtin_tools": {
      "browser": { "off_in_new_sessions": true },
      "terminal": { "enabled": false }
  } }
  ```

  `enabled: false` hides the group from every session (and from the menu);
  `off_in_new_sessions` makes new sessions start with it off. A group
  without an entry is on.
- **Project** (`<config_dir>/project-builtin-tools.json`): switching a group
  in a session is remembered for the session's project, so its next sessions
  start the same way. Only differences from the global default are kept.
- **Session** (`SessionConfig::enabled_builtin_tools` /
  `disabled_builtin_tools`): seeded at creation, changed from the input row's
  *Tools* menu (`SessionService::set_builtin_tool_enabled`), effective from
  the next agent run. Sessions without state follow the project/global
  default.

## Mechanism

`register_default_tools` tags each grouped tool with `toolset:<id>`
(`ToolRegistry::add_capability`). The agent adds the tags of the groups that
are off for its session to `excluded_tool_capabilities`, so they are neither
offered nor callable — sub-agents inherit this through the session config.
The tools stay registered so earlier calls of them still render.
