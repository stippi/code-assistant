# Code Assistant

[![CI](https://github.com/stippi/code-assistant/actions/workflows/build.yml/badge.svg)](https://github.com/stippi/code-assistant/actions/workflows/build.yml)

A CLI tool built in Rust for assisting with code-related tasks.

## Features

- **Autonomous Exploration**: The agent can intelligently explore codebases and build up working memory of the project structure.
- **Reading/Writing Files**: The agent can read file contents and make changes to files as needed.
- **User Interface**: The agent can run with a UI based on [Zed](https://zed.dev)'s [gpui](https://github.com/zed-industries/zed/tree/main/crates/gpui).
- **Interactive Communication**: Ability to ask users questions and get responses for better decision-making.
- **MCP Server Mode**: Can run as a Model Context Protocol server, providing tools and resources to LLMs running in an MCP client.

## Installation

Ensure you have [Rust installed](https://www.rust-lang.org/tools/install) on your system. Then:

```bash
# Clone the repository
git clone https://github.com/stippi/code-assistant

# Navigate to the project directory
cd code-assistant

# Build the project
cargo build --release

# The binary will be available in target/release/code-assistant
```

## Configuration in Claude Desktop

The `code-assistant` implements the [Model Context Protocol](https://modelcontextprotocol.io/introduction) by Anthropic.
This means it can be added as a plugin to MCP client applications such as **Claude Desktop**.

### Configure Your Projects

Create a file `~/.config/code-assistant/projects.json`.
This file adds available projects in MCP server mode (`list_projects` and file operation tools).
It has the following structure:

```json
{
  "code-assistant": {
    "path": "/Users/<username>/workspace/code-assistant"
  },
  "asteroids": {
    "path": "/Users/<username>/workspace/asteroids"
  },
  "zed": {
    "path": "Users/<username>/workspace/zed"
  }
}
```

Notes:
- The absolute paths are not provided by the tool, to avoid leaking such information to LLM cloud providers.
- This file can be edited without restarting Claude Desktop, respectively the MCP server.

### Configure MCP Servers

- Open the Claude Desktop application settings (**Claude** -> Settings)
- Switch to the **Developer** tab.
- Click the **Edit Config** button.

A Finder window opens highlighting the file `claude_desktop_config.json`.
Open that file in your favorite text editor.

An example configuration is given below:

```jsonc
{
  "mcpServers": {
    "code-assistant": {
      "command": "/Users/<username>/workspace/code-assistant/target/release/code-assistant",
      "args": [
        "server"
      ],
      "env": {
        "PERPLEXITY_API_KEY": "pplx-...", // optional, enables perplexity_ask tool
        "SHELL": "/bin/zsh" // your login shell, required when configuring "env" here
      }
    }
  }
}
```

## Usage

Code Assistant can run in two modes:

### Agent Mode (Default)

```bash
code-assistant --task <TASK> [OPTIONS]
```

Available options:
- `--path <PATH>`: Path to the code directory to analyze (default: current directory)
- `-t, --task <TASK>`: Task to perform on the codebase (required unless `--continue-task` or `--ui` is used)
- `--ui`: Start with GUI interface
- `--continue-task`: Continue from previous state
- `-v, --verbose`: Enable verbose logging
- `-p, --provider <PROVIDER>`: LLM provider to use [ai-core, anthropic, open-ai, ollama, vertex, openrouter] (default: anthropic)
- `-m, --model <MODEL>`: Model name to use (defaults: anthropic="claude-3-7-sonnet-20250219", open-ai="gpt-4o", vertex="gemini-2.5-pro-exp-03-25", openrouter="anthropic/claude-3-7-sonnet", ollama=required)
- `--base-url <URL>`: API base URL for the LLM provider
- `--tools-type <TOOLS_TYPE>`: Type of tool declaration [native, xml] (default: xml) `native` = tools via LLM provider API, `xml` = custom system message
- `--num-ctx <NUM>`: Context window size in tokens (default: 8192, only relevant for Ollama)
- `--agent-mode <MODE>`: Agent mode to use [working_memory, message_history] (default: message_history)
- `--record <PATH>`: Record API responses to a file for testing (currently supported for Anthropic and AI Core providers)
- `--playback <PATH>`: Play back a recorded session from a file
- `--fast-playback`: Fast playback mode - ignore chunk timing when playing recordings

Environment variables:
- `ANTHROPIC_API_KEY`: Required when using the Anthropic provider
- `OPENAI_API_KEY`: Required when using the OpenAI provider
- `GOOGLE_API_KEY`: Required when using the Vertex provider
- `OPENROUTER_API_KEY`: Required when using the OpenRouter provider
- `PERPLEXITY_API_KEY`: Required to use the Perplexity search API tools
- Note: AI Core authentication is configured on the command line (the tool will prompt for the parameters and store them in your default keychain)

Examples:
```bash
# Analyze code in current directory using Anthropic's Claude
code-assistant --task "Explain the purpose of this codebase"

# Continue a previously interrupted task
code-assistant --continue-task

# Start with GUI interface
code-assistant --ui

# Record a session for later playback
code-assistant --task "Optimize database queries" --record ./recordings/db-optimization.json

# Play back a recorded session with fast-forward (no timing delays)
code-assistant --playback ./recordings/db-optimization.json --fast-playback
```

### Server Mode

Runs as a Model Context Protocol server:

```bash
code-assistant server [OPTIONS]
```

Available options:
- `-v, --verbose`: Enable verbose logging

## Roadmap

This section is not really a roadmap, as the items are in no particular order.
Below are some topics that are likely the next focus.

- **Improve UI**: There are various ways in which the UI can be improved.
- **Persist Chats**: Persist all chats and make them available from a sidebar.
- **Add Memory Tools**: Add tools that facilitate building up a knowledge base useful work working in a given project.
- **Security**: Ideally, the execution for all tools would run in some sort of sandbox that restricts access to the files in the project tracked by git.
  Currently, the tools reject absolute paths, but do not check whether the relative paths point outside the project or try to access git-ignored files.
  The `execute_command` tool runs a shell with the provided command line, which at the moment is completely unchecked.
- **Fuzzy matching search blocks**: Investigate the benefit of fuzzy matching search blocks.
  Currently, files are normalized (always `\n` line endings, no trailing white space).
  This increases the success rate of matching search blocks quite a bit, but certain ways of fuzzy matching might increase the success even more.
  Failed matches introduce quite a bit of inefficiency, since they almost always trigger the LLM to re-read a file.
  Even when the error output of the `replace_in_file` tool includes the complete file and tells the LLM *not* to re-read the file.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.
