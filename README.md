# Code Assistant

[![CI](https://github.com/stippi/code-assistant/actions/workflows/build.yml/badge.svg)](https://github.com/stippi/code-assistant/actions/workflows/build.yml)
[![Trust Score](https://archestra.ai/mcp-catalog/api/badge/quality/stippi/code-assistant)](https://archestra.ai/mcp-catalog/stippi__code-assistant)

An open-source AI coding agent, written in Rust, with a native GUI, a terminal
mode, and integrations for editors and MCP clients. It runs an autonomous agent
loop over your codebase — reading, searching, editing files, and running
commands — while keeping you in the loop about what it is actually doing.

## Getting started

The quickest way to try it is a prebuilt download — no toolchain required:

1. Grab the latest build from the [**Releases page**](https://github.com/stippi/code-assistant/releases/latest):
   - **macOS:** `Code-Assistant-macos-aarch64.app.zip` (Apple Silicon) or `Code-Assistant-macos-x86_64.app.zip` (Intel)
   - **Linux:** `code-assistant-linux-x86_64.zip`
   - **Windows:** `code-assistant-windows-x86_64.zip`
2. Launch it. On first start, code-assistant opens its **Settings** screen.
   Pick a provider (there are one-click suggestions for the common ones), paste
   an API key, and choose a model — that's it.
3. Add your project folder from within the app and start a chat.

No hand-editing of JSON files required to get going. (You still can, if you
prefer — see the [Configuration guide](docs/configuration.md).)

<details>
<summary>Build from source instead</summary>

```bash
# Install the Rust toolchain (macOS/Linux)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# On Linux (Debian/Ubuntu), install the system libraries gpui needs:
sudo apt-get install -y --no-install-recommends \
    pkg-config build-essential libssl-dev libzstd-dev \
    libfontconfig-dev libwayland-dev libx11-xcb-dev \
    libxkbcommon-x11-dev libasound2-dev libvulkan1

# On macOS, install the metal toolchain:
xcodebuild -downloadComponent MetalToolchain

# Clone and build
git clone https://github.com/stippi/code-assistant
cd code-assistant
cargo build --release
```

The binary lands at `target/release/code-assistant`. See the
[Configuration guide](docs/configuration.md) for building a self-contained
macOS `.app` bundle.

</details>

## What makes it different

A handful of things we care about:

- **A UI you can actually follow.** Instead of a terse "Called 5 tools",
  code-assistant shows which tools ran and what each of them
  handed back to the model as context. When it goes off the rails, you can see
  why.
- **Format-on-save that stays token-efficient.** When your project auto-formats
  code, an agent's mental picture of a file goes stale and its follow-up edits
  start failing for non-obvious reasons. code-assistant runs your formatter and
  then reconciles the model's own edits with the formatted result — without
  wasting tokens on re-reading files. See
  [docs/format-on-save-feature.md](docs/format-on-save-feature.md).
- **Transparent file encoding & line endings.** It reads a file however it is
  stored (encoding, BOM, CRLF/LF), gives the model clean text, and writes it
  back the way it was — so it never silently rewrites your line endings.
- **Searches and reads documents, not just code.** Word, Excel, PowerPoint, 
  PDF and more are consumed automatically as Markdown, so you can point the 
  agent at real-world documents alongside your source.

## Features

- **Multiple LLM providers:** Anthropic, OpenAI, Google Vertex AI, Ollama,
  OpenRouter, SAP AI Core, Groq, Cerebras, Mistral, and more.
- **Four ways to work:** a native GUI (built on Zed's GPUI), a terminal mode, a
  headless MCP server, and an ACP agent for editors like [Zed](https://zed.dev).
- **Adaptive tool syntax:** native function calling, XML tags, or triple-caret
  blocks — chosen per session to fit the model.
- **Real-time streaming** with smart filtering that blocks unsafe tool
  combinations (e.g. editing a file before reading it).
- **Sessions per project** with branching, persistent state, and draft messages
  with attachments.
- **Sub-agents, permission tiers, a command sandbox,** and automatic context
  compaction when the window fills up.
- **MCP client mode:** plug in external Model Context Protocol servers and use
  their tools.
- **Skills:** reusable, task-specific playbooks the agent can load on demand.
- **Auto-loaded guidance:** picks up `AGENTS.md` (or `CLAUDE.md`) from your
  project root to align with repo-specific instructions.

## Interfaces

```bash
code-assistant                     # native GUI (default)
code-assistant --tui               # terminal interface
code-assistant acp                 # ACP agent for editors like Zed
code-assistant server              # headless MCP server (e.g. Claude Desktop)
```

Any mode can take an initial task: `code-assistant --task "Explain this codebase"`.

<details>
<summary>Connect to Zed (ACP)</summary>

Add to your Zed settings:

```json
{
  "agent_servers": {
    "Code-Assistant": {
      "command": "/path/to/code-assistant",
      "args": ["acp", "--model", "Claude Sonnet 4.5"],
      "env": { "ANTHROPIC_API_KEY": "sk-ant-..." }
    }
  }
}
```

See [Zed's docs on custom agents](https://zed.dev/docs/ai/external-agents#add-custom-agents).

</details>

<details>
<summary>Connect to Claude Desktop (MCP)</summary>

In Claude Desktop settings (**Developer** tab → **Edit Config**):

```json
{
  "mcpServers": {
    "code-assistant": {
      "command": "/path/to/code-assistant",
      "args": ["server"],
      "env": { "SHELL": "/bin/zsh" }
    }
  }
}
```

</details>

## Configuration

For most people the in-app **Settings** screen is enough — it manages providers,
models, MCP servers and skills for you. Everything can also be configured via
JSON files, and there are more advanced options (project setup, sandbox modes,
tool syntax, session recording, CLI flags):

**→ See the [Configuration guide](docs/configuration.md).**

## Contributing

Contributions are welcome! The codebase is a decent tour of async Rust, AI agent 
architecture, and cross-platform UI. If something about the agent's behaviour 
annoys you, that is exactly the kind of detail this project cares about: please
open an issue.

## Roadmap

Not really a roadmap — just a few directions, in no particular order:

- **Block replacing in stale files:** detect early when the model is about to
  `replace_in_file` a file that changed since it last read it, and reject with a
  helpful message.
- **Compact tool-use failures:** strip failed tool calls / mismatched search
  blocks from the history to save tokens.
- **Memory tools:** help the agent build up a knowledge base for a project.
- **Tighter sandboxing:** restrict tool access to git-tracked files within the
  project.
- **Fuzzy matching of search blocks:** reduce the re-reads caused by failed
  exact matches.
