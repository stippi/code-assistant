---
title: Configure
weight: 60
area: configure
lede: Most settings can be changed in the app's Settings screen. Every setting is also a JSON file you can edit by hand.
---

## Where configuration lives

All configuration files are in one folder:

1. the folder given with `--config-dir`, otherwise
2. `$CODE_ASSISTANT_CONFIG_DIR`, otherwise
3. `$XDG_CONFIG_HOME/code-assistant`, otherwise
4. `~/.config/code-assistant`

Sessions, goals and per-session UI state are data, not configuration. They are
kept in `$CODE_ASSISTANT_DATA_DIR`, or in the platform's data folder
(`~/Library/Application Support/code-assistant` on macOS).

Files you will typically touch:

| File | What it holds | Settings screen |
|---|---|---|
| `providers.json` | API keys and endpoints | Providers |
| `model-selection.json` | Which models the picker offers, and per-model overrides | Models |
| `projects.json` | Your projects and their format-on-save rules | (sidebar → *New project*) |
| `mcp-servers.json` | MCP servers | MCP Servers |
| `tools.json` | Built-in tool groups, API keys for web tools | Tools |
| `skills.json` | Skills on/off | Skills |
| `voice.json` | Voice mode | Voice |
| `AGENTS.md` | Guidance added to every session's system prompt | (none) |

Values like `"${ANTHROPIC_API_KEY}"` are replaced with the environment
variable of that name, so keys don't have to be stored in the files.
