# Code Assistant

A CLI tool built in Rust for assisting with code-related tasks.

## Features

- **Autonomous Exploration**: The agent can intelligently explore codebases and build up working memory of the project structure.
- **Reading/Writing Files**: The agent can read file contents and make changes to files as needed.
- **Working Memory Management**: Efficient handling of file contents with the ability to load and unload files from memory.
- **File Summarization**: Capability to create and store file summaries for quick reference and better understanding of the codebase.
- **Interactive Communication**: Ability to ask users questions and get responses for better decision-making.
- **MCP Server Mode**: Can run as a Model Context Protocol server, providing tools and resources to LLMs running in an MCP client.

## Installation

Ensure you have Rust installed on your system. Then:

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

The `code-assistant` implements the [Model Context Protocol]() by Anthropic.
This means it can be added as a plugin to MCP client applications such as **Claude Desktop**.

### Configure Your Projects

Create a file `.code-assistant/projects.json` in your home directory.
This file adds available projects in MCP server mode (`list_projects` and `open_project` tools).
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

## Agent Mode Usage

```bash
code-assistant agent --task <TASK> [OPTIONS]
```
Available options:
- `--path <PATH>`: Path to the code directory to analyze (default: current directory)
- `-t, --task <TASK>`: Required. The task to perform on the codebase
- `-v, --verbose`: Enable verbose logging
- `-p, --provider <PROVIDER>`: LLM provider to use [anthropic, openai, ollama, vertex] (default: anthropic)
- `-m, --model <MODEL>`: Model name to use (provider-specific)
- `--tools-type <TOOLS_TYPE>`: Type of tool declaration [native, xml] (default: xml) `native` = tools via LLM provider API, `xml` = custom system message
- `--num-ctx <NUM>`: Context window size in tokens (default: 8192, only relevant for Ollama)
Environment variables:
- `ANTHROPIC_API_KEY`: Required when using the Anthropic provider
- `OPENAI_API_KEY`: Required when using the OpenAI provider
- `GOOGLE_API_KEY`: Required when using the Vertex provider
Example:
```bash
# Analyze code in current directory using Anthropic's Claude
code-assistant agent --task "Explain the purpose of this codebase"
# Use OpenAI to analyze a specific directory with verbose logging
code-assistant agent -p openai --path ./my-project -t "List all API endpoints" -v
# Use Google's Vertex AI with a specific model
code-assistant agent -p vertex --model gemini-1.5-flash -t "Analyze code complexity"
```

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.
