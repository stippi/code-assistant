---
title: Command-line options
linkTitle: CLI options
weight: 10
lede: Every mode and flag of the code-assistant binary.
frontends: [app, tui, acp]
---

## Modes

```bash
code-assistant                 # the app (GUI)
code-assistant --tui           # terminal interface
code-assistant acp             # ACP agent for editors
code-assistant server          # MCP server over stdio
```

## Options for the app and the terminal

| Option | Meaning |
|---|---|
| `--tui` | Start the terminal interface instead of the app |
| `-t`, `--task <text>` | Start with this task |
| `-m`, `--model <name or key>` | Model to use, by display name or `provider/model-id` |
| `--path <dir>` | Project folder (default: the current folder) |
| `--continue-task` | Continue the previous session |
| `--tool-syntax native\|xml\|caret` | How tools are called (default `native`) |
| `--use-diff-format` | Edit with `replace_in_file` diff blocks instead of `edit` |
| `--sandbox-mode danger-full-access\|workspace-write\|read-only` | Command sandbox (default `danger-full-access`) |
| `--sandbox-network` | Allow network access in `workspace-write` |
| `--config-dir <dir>` | Use another configuration folder |
| `-v`, `--verbose` | More logging. Repeat for even more (`-vv`) |
| `--list-models`, `--list-providers` | Print what is configured and exit |
| `--record <file>` | Record the API responses to a file (Anthropic only) |
| `--playback <file>`, `--fast-playback` | Replay a recording, optionally without its timing |

`acp` takes `--model`, `--path`, `--tool-syntax`, `--use-diff-format`,
`--sandbox-mode`, `--sandbox-network` and `--verbose`. `server` takes `--verbose`.

## Login commands

| Command | Does |
|---|---|
| `code-assistant codex-login` / `codex-logout` / `codex-status` | ChatGPT subscription login |
| `code-assistant mcp-login <server> [--path <project>]` | OAuth login for an HTTP MCP server (also one from a trusted `.mcp.json`) |
| `code-assistant mcp-logout <server>` | Remove the stored logins of a server |

## Environment variables

| Variable | Meaning |
|---|---|
| `CODE_ASSISTANT_CONFIG_DIR` | Configuration folder (like `--config-dir`) |
| `CODE_ASSISTANT_DATA_DIR` | Data folder for sessions and goals |
| Any name used as `${NAME}` | Substituted in config values (API keys, MCP `env` and `headers`) |
