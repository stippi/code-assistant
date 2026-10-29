# Configuration Guide

For most people the in-app **Settings** screen (which opens automatically on
first launch of the GUI) is enough: it manages providers, models, MCP servers
and skills without touching a single file. This guide is for everything beyond
that — manual JSON configuration, project setup, and advanced options.

Configuration lives in `~/.config/code-assistant/`.

## Providers and models

Providers hold credentials and endpoints; which of their models you can pick,
and how hard a model thinks, is mostly worked out by the app. The Settings
screen writes both files for you; you can also edit them by hand.

### `providers.json` — credentials and endpoints

```json
{
  "anthropic-main": {
    "label": "Anthropic Claude",
    "provider": "anthropic",
    "config": {
      "api_key": "${ANTHROPIC_API_KEY}",
      "base_url": "https://api.anthropic.com/v1"
    }
  },
  "openai-chatgpt": {
    "label": "ChatGPT Subscription",
    "provider": "openai-responses-ws",
    "config": {
      "codex_auth": true
    }
  }
}
```

Use `${VAR_NAME}` to reference environment variables for API keys. Each entry's
top-level key (e.g. `anthropic-main`) is the provider ID models refer to.

### Which models the picker offers — `model-selection.json`

*Settings → Models* lists, per provider, the models the provider offers (its
`/models` listing, cached in `model-discovery.json` and refreshed daily or on
demand). Switch a model on to offer it in the model picker. A model is
referred to by its key, `<provider-id>/<model-id>`.

Everything else comes from a small built-in catalog and the provider's
listing: the context window, the output limit, the preferred edit format and
the **reasoning effort levels** the model supports, including how the provider
wants them in a request (`thinking`/`output_config.effort` for Anthropic,
`reasoning.effort` for the Responses API, `thinking_level` for Gemini, ...).
The effort is chosen per session next to the model picker (`/effort` in the
terminal UI, a *thought level* option in ACP clients).

An entry only holds what you override:

```json
{
  "models": [
    { "provider": "anthropic-main", "id": "claude-opus-5-5" },
    { "provider": "anthropic-main", "id": "claude-sonnet-5-5", "default_effort": "medium" },
    {
      "provider": "ai-core-dev",
      "id": "anthropic--claude-4.8-opus",
      "name": "Claude Opus 4.8 (AI Core)",
      "behaves_like": "claude-opus-4-8"
    },
    {
      "provider": "ollama-local",
      "id": "qwen3.8:27b-mlx",
      "context_token_limit": 128000,
      "config": { "temperature": 0.7 }
    }
  ]
}
```

- `name` — display name (default: the catalog's or provider's name)
- `behaves_like` — catalog model to take capabilities from, for IDs the catalog
  does not know (e.g. a proxy's name for a known model)
- `context_token_limit`, `edit_format` (`simple`/`diff`) — overrides
- `default_effort` — effort for sessions that did not choose one
- `config` — extra request fields, merged into every request as a JSON Merge
  Patch (objects merge, `null` removes a field the client sends)

As long as there is no `model-selection.json`, the entries of an old
`models.json` are imported: their names stay (sessions refer to them), their
reasoning settings become the `default_effort`. The new file is written with
your first change in *Settings → Models*; the old file is left as it is.

See [`providers.example.json`](../providers.example.json) and
[`model-selection.example.json`](../model-selection.example.json).

**List what is configured:**

```bash
code-assistant --list-models
code-assistant --list-providers
```

## Tool settings — `tools.json`

Some tools need external API keys:

```json
{
  "perplexity_api_key": "${PERPLEXITY_API_KEY}"
}
```

- `perplexity_api_key` — enables the `perplexity_ask` tool for AI-powered web search.

Tools without their required configuration are simply not offered to the agent.

## Project configuration — `projects.json`

Define the projects the assistant can work on:

```jsonc
{
  "code-assistant": {
    "path": "/Users/<username>/workspace/code-assistant",
    "format_on_save": {
      "**/*.rs": "cargo fmt"
    }
  },
  "my-project": {
    "path": "/Users/<username>/workspace/my-project",
    "format_on_save": {
      "**/*.ts": "prettier --write {path}"
    }
  }
}
```

Notes:

- Launching from a folder not listed here creates a temporary project
  automatically. The assistant has access to the current project plus all
  configured ones.
- Each chat session is permanently tied to its initial project/folder and tool
  syntax; these cannot be changed later.
- **Working directory matters** in terminal/CLI use: the directory you launch
  from scopes file operations and search, and chats are grouped by directory.
  `cd` into your project root before starting.

### Format-on-save

The optional `format_on_save` field maps glob patterns to shell commands run
after the assistant modifies a matching file. The tool parameters are updated to
reflect the formatted content, keeping the model's mental model in sync and
avoiding edit conflicts. Use the `{path}` placeholder for formatters that take a
file argument (e.g. `prettier --write {path}`); omit it for whole-project
formatters (e.g. `cargo fmt`). Full details in
[format-on-save-feature.md](format-on-save-feature.md).

## MCP client mode

code-assistant can connect to external Model Context Protocol servers and
register their tools. Configure them via the Settings screen ("MCP Servers") or
in `~/.config/code-assistant/mcp-servers.json` (per-server `enabled`,
`enabled_tools` allowlist, `disabled_tools` denylist, and `${ENV_VAR}`
substitution in `env`/`headers` values). Configuration changes apply on the
next agent run — no restart required. A running agent keeps the tool set it
started with; the next message picks up added, removed or re-configured servers.

Each server is reached over one of two transports, selected by the fields you
give it — a `command` runs it as a child process over stdio, a `url` connects
over HTTP (streamable transport):

```json
{
  "servers": {
    "jira": {
      "command": "npx",
      "args": ["-y", "some-jira-server"],
      "env": { "JIRA_TOKEN": "${JIRA_TOKEN}" }
    },
    "remote": {
      "url": "https://example.com/mcp",
      "headers": { "Authorization": "Bearer ${REMOTE_TOKEN}" }
    }
  }
}
```

## Plugins

Plugins add provider templates, read-only MCP servers and file viewers (such
as draw.io for `.drawio` files). Install them in *Settings → Plugins* or with
`code-assistant plugin add <git-url | owner/repo[#ref] | folder>`; they live
in `~/.config/code-assistant/plugins/`. See [plugins.md](plugins.md) and the
example in [`examples/plugin`](../examples/plugin).

## Advanced CLI options

<details>
<summary>Tool syntax modes</summary>

- `--tool-syntax native` — the provider's built-in tool calling (most reliable;
  parameter streaming depends on the provider)
- `--tool-syntax xml` — XML-style tags, with parameter streaming
- `--tool-syntax caret` — triple-caret blocks, token-efficient, with streaming

</details>

<details>
<summary>Session recording &amp; playback (Anthropic)</summary>

```bash
code-assistant --record session.json --model "Claude Sonnet 4.5" --task "Optimize database queries"
code-assistant --playback session.json --fast-playback
```

</details>

<details>
<summary>Other flags</summary>

- `--model <name>` — pick a model by name or key (`--list-models` to see them)
- `--task <text>` — start with an initial task
- `--tui` — terminal interface
- `--continue-task` — resume from previous session state
- `--use-diff-format` — alternative diff format for file editing
- `--sandbox-mode <danger-full-access|read-only|workspace-write>` — command
  execution sandbox policy (default `danger-full-access`)
- `--sandbox-network` — with `--sandbox-mode workspace-write`, allow outbound
  network access inside the sandbox
- `--verbose` / `-v` — detailed logging (repeat for more)

</details>

## Building a macOS `.app` bundle from source

A self-contained bundle (dock icon, `Info.plist`, ad-hoc code signature) can be
built from any release binary using only stock macOS tools:

```bash
# Build the binary first (per-target)
cargo build --locked --release --target aarch64-apple-darwin   # Apple Silicon
# or
cargo build --locked --release --target x86_64-apple-darwin    # Intel

# Wrap it into a .app bundle
./scripts/bundle-macos.sh aarch64
# Other options: x86_64, universal, --no-build (reuse an existing binary)
```

The result lands in `target/macos-bundle/Code Assistant.app` plus a zipped copy.
The icon source is `crates/code_assistant/assets/app_icon.svg`; re-run
`./scripts/generate-app-icon.sh` after editing it to refresh `AppIcon.icns`.
