# Code Assistant

A powerful CLI tool built in Rust for assisting with code-related tasks.

## Features

- **Autonomous Exploration**: The agent can intelligently explore codebases and build up working memory of the project structure.
- **Reading/Writing Files**: The agent can read file contents and make changes to files as needed.
- **Working Memory Management**: Efficient handling of file contents with the ability to load and unload files from memory.
- **File Summarization**: Capability to create and store file summaries for quick reference and better understanding of the codebase.
- **Interactive Communication**: Built-in ability to ask users questions and get responses for better decision-making.
- **MCP Server Mode**: Can run as a Model Context Protocol server, providing tools and resources to LLMs through standard interfaces.

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

## Usage

```bash
code-assistant --task <TASK> [OPTIONS]
```
Available options:
- `--path <PATH>`: Path to the code directory to analyze (default: current directory)
- `-t, --task <TASK>`: Required. The task to perform on the codebase
- `-v, --verbose`: Enable verbose logging
- `-p, --provider <PROVIDER>`: LLM provider to use [anthropic, openai, ollama] (default: anthropic)
- `-m, --model <MODEL>`: Model name to use (provider-specific)
- `--num-ctx <NUM>`: Context window size in tokens (default: 8192, only relevant for Ollama)
Environment variables:
- `ANTHROPIC_API_KEY`: Required when using the Anthropic provider
- `OPENAI_API_KEY`: Required when using the OpenAI provider
Example:
```bash
# Analyze code in current directory using Anthropic's Claude
code-assistant --task "Explain the purpose of this codebase"
# Use OpenAI to analyze a specific directory with verbose logging
code-assistant -p openai --path ./my-project -t "List all API endpoints" -v
```

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.
