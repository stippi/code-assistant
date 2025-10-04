# Code Assistant

[![CI](https://github.com/stippi/code-assistant/actions/workflows/build.yml/badge.svg)](https://github.com/stippi/code-assistant/actions/workflows/build.yml)

An AI coding assistant built in Rust that provides both command-line and graphical interfaces for autonomous code analysis and modification.

## Key Features

**Multi-Modal Tool Execution**: Adapts to different LLM capabilities with pluggable tool invocation modes - native function calling, XML-style tags, and triple-caret blocks - ensuring compatibility across various AI providers.

**Real-Time Streaming Interface**: Advanced streaming processors parse and display tool invocations as they stream from the LLM, with smart filtering to prevent unsafe tool combinations.

**Session-Based Project Management**: Each chat session is tied to a specific project and maintains persistent state, working memory, and draft messages with attachment support.

**Multiple Interface Options**: Choose between a modern GUI built on Zed's GPUI framework, traditional terminal interface, or headless MCP server mode for integration with MCP clients such as Claude Desktop.

**Agent Client Protocol (ACP) Support**: Full compatibility with the [Agent Client Protocol](https://agentclientprotocol.com/) standard, enabling seamless integration with ACP-compatible editors like [Zed](https://zed.dev). See Zed's documentation on [adding custom agents](https://zed.dev/docs/ai/external-agents#add-custom-agents) for setup instructions.

**Intelligent Project Exploration**: Autonomously builds understanding of codebases through working memory that tracks file structures, dependencies, and project context.

**Auto-Loaded Repository Guidance**: Automatically includes `AGENTS.md` (or `CLAUDE.md` fallback) from the project root in the assistant's system context to align behavior with repo-specific instructions.

## Installation

```bash
git clone https://github.com/stippi/code-assistant
cd code-assistant
cargo build --release
```

The binary will be available at `target/release/code-assistant`.

## Project Configuration

Create `~/.config/code-assistant/projects.json` to define available projects:

```jsonc
{
  "code-assistant": {
    "path": "/Users/<username>/workspace/code-assistant",
    "format_on_save": {
      "**/*.rs": "cargo fmt" // Formats all files in project, so make sure files are already formatted
    }
  },
  "my-project": {
    "path": "/Users/<username>/workspace/my-project",
    "format_on_save": {
      "**/*.ts": "prettier --write {path}" // If the formatter accepts a path, provide "{path}"
    }
  }
}
```

### Format-on-Save Feature

The _optional_ `format_on_save` field allows automatic formatting of files after modifications. It maps file patterns (using glob syntax) to shell commands:
- Files matching the glob patterns will be automatically formatted after being modified by the assistant
- The tool parameters are updated to reflect the formatted content, keeping the LLM's mental model in sync
- This prevents edit conflicts caused by auto-formatting

See [docs/format-on-save-feature.md](docs/format-on-save-feature.md) for detailed documentation.

**Important Notes:**
- When launching from a folder not in this configuration, a temporary project is created automatically
- The assistant has access to the current project (including temporary ones) plus all configured projects
- Each chat session is permanently associated with its initial project and folder - this cannot be changed later
- Tool syntax (native/xml/caret) is also fixed per session at creation time
- The LLM provider selected at startup is used for the entire application session (UI switching planned for future releases)

## Usage

### GUI Mode (Recommended)

```bash
# Start with graphical interface
code-assistant --ui

# Start GUI with initial task
code-assistant --ui --task "Analyze the authentication system"
```

### Terminal Mode

```bash
# Basic usage
code-assistant --task "Explain the purpose of this codebase"

# With specific provider and model
code-assistant --task "Add error handling" --provider openai --model gpt-5
```

### MCP Server Mode

```bash
code-assistant server
```

### ACP Agent Mode

```bash
# Run as ACP-compatible agent
code-assistant acp

# With specific provider and model
code-assistant acp --provider openai --model gpt-5-codex
```

The ACP mode enables integration with editors that support the [Agent Client Protocol](https://agentclientprotocol.com/), such as [Zed](https://zed.dev). When running in ACP mode, the code-assistant communicates via JSON-RPC over stdin/stdout, supporting features like pending messages, real-time streaming, and tool execution with proper permission handling.

## Configuration

<details>
<summary>Claude Desktop Integration</summary>

Configure in Claude Desktop settings (**Developer** tab → **Edit Config**):

```jsonc
{
  "mcpServers": {
    "code-assistant": {
      "command": "/path/to/code-assistant/target/release/code-assistant",
      "args": ["server"],
      "env": {
        "PERPLEXITY_API_KEY": "pplx-...", // optional, enables perplexity_ask tool
        "SHELL": "/bin/zsh" // your login shell, required when configuring "env" here
      }
    }
  }
}
```
</details>

<details>
<summary>LLM Providers</summary>

**Anthropic** (default):
```bash
export ANTHROPIC_API_KEY="sk-ant-..."
code-assistant --provider anthropic --model claude-sonnet-4-20250514
```

**OpenAI**:
```bash
export OPENAI_API_KEY="sk-..."
code-assistant --provider openai --model gpt-4o
```

**SAP AI Core**:
Create `~/.config/code-assistant/ai-core.json`:
```json
{
  "auth": {
    "client_id": "<service-key-client-id>",
    "client_secret": "<service-key-client-secret>",
    "token_url": "https://<your-url>/oauth/token",
    "api_base_url": "https://<your-url>/v2/inference"
  },
  "models": {
    "claude-sonnet-4": "<deployment-id>"
  }
}
```

**Ollama**:
```bash
code-assistant --provider ollama --model llama2 --num-ctx 4096
```

**Other providers**: Vertex AI (Google), OpenRouter, Groq, MistralAI
</details>

<details>
<summary>Zed Editor Integration (ACP)</summary>

Configure in Zed settings (**Extensions** → **Installed** → **Assistant** → **Settings**):

```json
{
  "agent_servers": {
    "Code-Assistant": {
      "command": "/path/to/code-assistant/target/release/code-assistant",
      "args": [
        "acp",
        "--provider",
        "anthropic",
        "--model",
        "claude-sonnet-4-5"
      ],
      "env": {
        "ANTHROPIC_API_KEY": "sk-ant-..."
      }
    }
  }
}
```

Replace the path with your actual installation location and configure your preferred provider. The agent will appear in Zed's assistant panel and support all ACP features including pending messages, real-time streaming, and interactive tool execution.

For detailed setup instructions, see [Zed's documentation on adding custom agents](https://zed.dev/docs/ai/external-agents#add-custom-agents).
</details>

<details>
<summary>Advanced Options</summary>

**Tool Syntax Modes**:
- `--tool-syntax native`: Use the provider's built-in tool calling (most reliable, but streaming of parameters depends on provider)
- `--tool-syntax xml`: XML-style tags for streaming of parameters
- `--tool-syntax caret`: Triple-caret blocks for token-efficency and streaming of parameters

**Session Recording**:
```bash
# Record session (Anthropic only)
code-assistant --record session.json --task "Optimize database queries"

# Playback session
code-assistant --playback session.json --fast-playback
```

**Other Options**:
- `--continue-task`: Resume from previous session state
- `--use-diff-format`: Enable alternative diff format for file editing
- `--verbose`: Enable detailed logging
- `--base-url`: Custom API endpoint
</details>

## Architecture Highlights

The code-assistant features several innovative architectural decisions:

**Adaptive Tool Syntax**: Automatically generates different system prompts and streaming processors based on the target LLM's capabilities, allowing the same core logic to work across providers with varying function calling support.

**Smart Tool Filtering**: Real-time analysis of tool invocation patterns prevents logical errors like attempting to edit files before reading them, with the ability to truncate responses mid-stream when unsafe combinations are detected.

**Multi-Threaded Streaming**: Sophisticated async architecture that handles real-time parsing of tool invocations while maintaining responsive UI updates and proper state management across multiple chat sessions.

## Contributing

Contributions are welcome! The codebase demonstrates advanced patterns in async Rust, AI agent architecture, and cross-platform UI development.

## Roadmap

This section is not really a roadmap, as the items are in no particular order.
Below are some topics that are likely the next focus.

- **Block Replacing in Changed Files**: When streaming a tool use block, we already know the LLM attempts to use `replace_in_file` and we know in which file quite early.
  If we also know this file has changed since the LLM last read it, we can block the attempt with an appropriate error message.
- **Compact Tool Use Failures**: When the LLM produces an invalid tool call, or a mismatching search block, we should be able to strip the failed attempt from the message history, saving tokens.
- **Improve UI**: There are various ways in which the UI can be improved.
- **Add Memory Tools**: Add tools that facilitate building up a knowledge base useful work working in a given project.
- **Security**: Ideally, the execution for all tools would run in some sort of sandbox that restricts access to the files in the project tracked by git.
  Currently, the tools reject absolute paths, but do not check whether the relative paths point outside the project or try to access git-ignored files.
  The `execute_command` tool runs a shell with the provided command line, which at the moment is completely unchecked.
- **Fuzzy matching search blocks**: Investigate the benefit of fuzzy matching search blocks.
  Currently, files are normalized (always `\n` line endings, no trailing white space).
  This increases the success rate of matching search blocks quite a bit, but certain ways of fuzzy matching might increase the success even more.
  Failed matches introduce quite a bit of inefficiency, since they almost always trigger the LLM to re-read a file.
  Even when the error output of the `replace_in_file` tool includes the complete file and tells the LLM *not* to re-read the file.
- **Edit user messages**: Editing a user message should create a new branch in the session.
  The user should still be able to toggle the active banches.
- **Select in messages**: Allow to copy/paste from any message in the session.
