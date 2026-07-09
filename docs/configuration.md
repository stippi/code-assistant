# Configuration Guide

For most people the in-app **Settings** screen (which opens automatically on
first launch of the GUI) is enough: it manages providers, models, MCP servers
and skills without touching a single file. This guide is for everything beyond
that — manual JSON configuration, project setup, and advanced options.

Configuration lives in `~/.config/code-assistant/`.

## Providers and models

Two JSON files manage LLM access. The Settings screen writes these for you; you
can also edit them by hand.

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
top-level key (e.g. `anthropic-main`) is an alias that models reference via their
`provider` field.

### `models.json` — available models

```json
{
  "Claude Opus 4.8": {
    "provider": "anthropic-main",
    "id": "claude-opus-4-7",
    "context_token_limit": 200000,
    "edit_format": "diff",
    "config": {
      "max_tokens": 64000,
      "thinking": { "type": "adaptive", "display": "summarized" },
      "output_config": { "effort": "medium" }
    }
  },
  "Claude Fable 5": {
    "provider": "anthropic-main",
    "id": "claude-fable-5",
    "context_token_limit": 1000000,
    "edit_format": "diff",
    "config": {
      "max_tokens": 64000,
      "thinking": { "type": "adaptive", "display": "summarized" },
      "output_config": { "effort": "high" }
    }
  },
  "GPT-5.5 (ChatGPT)": {
    "provider": "openai-chatgpt",
    "id": "gpt-5.5",
    "context_token_limit": 400000,
    "config": {
      "temperature": 1.0,
      "reasoning": { "context": "all_turns", "effort": "high", "summary": "concise" }
    }
  }
}
```

Common per-model fields: `context_token_limit` sizes the context window,
`edit_format` selects the file-editing format (e.g. `diff`), and the `config`
block carries provider-specific options such as `thinking` / `output_config`
(Anthropic) or `reasoning` (OpenAI).

**Full examples** for every supported provider (Anthropic, OpenAI, Ollama,
SAP AI Core, Vertex AI, Groq, Cerebras, Mistral, OpenRouter) live in
[`providers.example.json`](../providers.example.json) and
[`models.example.json`](../models.example.json).

**List what is configured:**

```bash
code-assistant --list-models
code-assistant --list-providers
```

### Note on Claude Opus 4.7+ extended thinking

Starting with Claude Opus 4.7, Anthropic no longer accepts the manual
`thinking: { type: "enabled", budget_tokens: N }` form (it returns a 400 error).
These models require *adaptive* thinking, where depth is controlled via
`output_config.effort` (`low`, `medium`, `high`, `xhigh`, `max`):

```json
{
  "Claude Opus 4.8": {
    "provider": "anthropic-main",
    "id": "claude-opus-4-7",
    "config": {
      "max_tokens": 64000,
      "thinking": { "type": "adaptive", "display": "summarized" },
      "output_config": { "effort": "medium" }
    }
  }
}
```

code-assistant detects Opus 4.7+ model IDs (`claude-opus-4-7`, `claude-opus-4-8`,
`claude-opus-latest`) and emits the correct request shape by default. See
Anthropic's [extended thinking](https://docs.anthropic.com/en/docs/build-with-claude/extended-thinking)
and [effort](https://docs.anthropic.com/en/docs/build-with-claude/effort) docs.

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
substitution in `env` values). Servers connect at process start; config changes
require a restart.

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

- `--model <name>` — pick a model from `models.json` (`--list-models` to see them)
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
