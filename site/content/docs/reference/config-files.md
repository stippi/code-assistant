---
title: Config files
weight: 40
lede: Every file in the configuration folder, what writes it, and where it's explained.
frontends: [app, tui, acp]
---

The configuration folder is `~/.config/code-assistant/` unless set otherwise
(see [Configure](/docs/configure/)).

| File | Holds | Written by | Guide |
|---|---|---|---|
| `providers.json` | Providers: type, credentials, endpoints | Settings → Providers | [Providers and models](/docs/configure/providers/) |
| `model-selection.json` | Models offered in the picker, per-model overrides | Settings → Models | [Providers and models](/docs/configure/providers/#model-selectionjson) |
| `model-discovery.json` | Cached model listings of the providers | the app | (none) |
| `default-model.json` | The default model for new sessions | Settings → General | (none) |
| `projects.json` | Projects and format-on-save | Sidebar → New project | [Projects](/docs/configure/projects/) |
| `mcp-servers.json` | MCP servers | Settings → MCP Servers | [MCP servers](/docs/configure/mcp/) |
| `mcp-trust.json` | Trusted project `.mcp.json` files | trust prompt | [Trust](/docs/configure/mcp/#trust) |
| `mcp-oauth/` | OAuth logins of MCP servers | login | [MCP servers](/docs/configure/mcp/) |
| `project-mcp-servers.json` | Per-project server choices | Tools menu | [MCP servers](/docs/configure/mcp/) |
| `tools.json` | Built-in tool groups, `perplexity_api_key` | Settings → Tools | [Built-in tools](/docs/configure/tools/) |
| `project-builtin-tools.json` | Per-project tool group choices | Tools menu | [Built-in tools](/docs/configure/tools/) |
| `skills.json` | Skills on/off | Settings → Skills | [Skills](/docs/configure/skills/) |
| `skills/` | Your own skills | you | [Skills](/docs/configure/skills/) |
| `voice.json` | Voice mode | Settings → Voice | [Voice mode](/docs/organize/voice/) |
| `AGENTS.md` | Guidance for every session | you | [AGENTS.md](/docs/configure/skills/) |
| `handoff.json` | Threshold for the prepared handoff when idle | Settings → General | [Fresh context](/docs/long/context/) |
| `lifecycle.json` | When sessions settle (idle days, on merge) | Settings → General | [Settling](/docs/organize/sessions/#settle) |
| `browser-profiles/` | Saved browser logins | `browser_login` | [Browser](/docs/work/browser/#login) |
| `ui-settings.json` | Theme, zoom, sidebar state | the app | (none) |

In a project:

| File | Holds |
|---|---|
| `AGENTS.md` or `CLAUDE.md` | Guidance for sessions in this project |
| `.agents/skills/` | Project skills |
| `.mcp.json` | Project MCP servers (asks for trust first) |
