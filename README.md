# Code Assistant

[![CI](https://github.com/stippi/code-assistant/actions/workflows/build.yml/badge.svg)](https://github.com/stippi/code-assistant/actions/workflows/build.yml)

A CLI tool built in Rust for assisting with code-related tasks.

## Features

- **Autonomous Exploration**: The agent can intelligently explore codebases and build up working memory of the project structure.
- **Reading/Writing Files**: The agent can read file contents and make changes to files as needed.
- **Working Memory Management**: Efficient handling of file contents with the ability to load and unload files from memory.
- **File Summarization**: Capability to create and store file summaries for quick reference and better understanding of the codebase.
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

```json
{
  "mcpServers": {
    "code-assistant": {
      "command": "/Users/<username>/workspace/code-assistant/target/release/code-assistant",
      "args": [
        "server"
      ]
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
- Note: AI Core authentication is configured on the command line (the tool will prompt for the parameters and store them in your default keychain)

Examples:
```bash
# Analyze code in current directory using Anthropic's Claude
code-assistant --task "Explain the purpose of this codebase"

# Use OpenAI to analyze a specific directory with verbose logging
code-assistant -p open-ai --path ./my-project -t "List all API endpoints" -v

# Use Google's Vertex AI with a specific model
code-assistant -p vertex --model gemini-1.5-flash -t "Analyze code complexity"

# Use Ollama with a specific model (model is required for Ollama)
code-assistant -p ollama -m codellama --task "Find all TODO comments in the codebase"

# Use AI Core provider
code-assistant -p ai-core --task "Document the public API"

# Use with working memory agent mode instead of message history mode
code-assistant --task "Find performance bottlenecks" --agent-mode working_memory

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

- **UI improvements**: The text input for the user message is horrible. There is currently no markdown support or syntax highlighting for code blocks. There is a project [longbridge/gpui-component](https://github.com/longbridge/gpui-component) with a component library building on top of Zed's GPUI crate. It contains a lot of useful components and the license is more permissive than Zed's own components.
- **Agent improvements**: The working memory mode is not what LLMs are trained for and thus it doesn't work so well. Too many tokens are generated before calling the next tool. In the chat message history mode on the other hand, the total input token count can quickly grow out of hand. Especially when the messages contain multiple redundant copies of the exact same resources. I would like to explore ways to automatically prune the messages to avoid that.
- **Agent native tool mode**: The stream filtering that detects different types of content blocks (thinking, output, tool calls, parameters), is currently optimized for the XML mode and should be improved for the native tools.
- **Better search tool**: The current web search tool is very basic and doesn't work so well. Maybe an alternative tool using the Perplexity API would work better.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.
