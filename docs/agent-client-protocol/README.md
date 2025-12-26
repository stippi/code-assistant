# Agent Client Protocol (ACP) Documentation

This directory contains documentation about the Agent Client Protocol (ACP).

## Overview

The Agent Client Protocol standardizes communication between code editors (IDEs, text-editors, etc.) and coding agents (programs that use generative AI to autonomously modify code). It is suitable for both local and remote scenarios.

## Why ACP?

AI coding agents and editors are tightly coupled but interoperability isn't the default. Each editor must build custom integrations for every agent they want to support, and agents must implement editor-specific APIs to reach users. This creates several problems:

- **Integration overhead**: Every new agent-editor combination requires custom work
- **Limited compatibility**: Agents work with only a subset of available editors
- **Developer lock-in**: Choosing an agent often means accepting their available interfaces

ACP solves this by providing a standardized protocol for agent-editor communication, similar to how the Language Server Protocol (LSP) standardized language server integration.

## Key Concepts

- **Agents**: Programs that use generative AI to autonomously modify code. They typically run as subprocesses of the Client (local) or as remote services.
- **Clients**: Code editors (IDEs, text editors) that provide the interface between users and agents. They manage the environment, handle user interactions, and control access to resources.
- **Sessions**: Independent conversation contexts with their own history and state
- **Communication**: JSON-RPC 2.0 over stdio (local) or HTTP/WebSocket (remote)
- **Content Format**: Markdown for user-readable text

## Documentation Structure

1. [Introduction](./01-introduction.md) - Basic concepts and protocol overview
2. [Protocol Overview](./02-protocol-overview.md) - Communication model and message flow
3. [Initialization](./03-initialization.md) - Version negotiation and capability exchange
4. [Session Setup](./04-session-setup.md) - Creating and loading sessions
5. [Prompt Turn](./05-prompt-turn.md) - The complete lifecycle of a user prompt
6. [Tool Calls](./06-tool-calls.md) - How agents execute operations
7. [Content Types](./07-content-types.md) - Different content block types
8. [File System](./08-file-system.md) - Reading and writing files
9. [Terminals](./09-terminals.md) - Executing shell commands
10. [Agent Plan](./10-agent-plan.md) - Execution plans for complex tasks
11. [Rust Implementation](./11-rust-implementation.md) - Using the Rust crate
12. [Session Modes](./12-session-modes.md) - Agent operating modes
13. [Slash Commands](./13-slash-commands.md) - User-invokable commands
14. [Extensibility](./14-extensibility.md) - Custom extensions and _meta fields

## Resources

- Official Website: https://agentclientprotocol.com
- Rust Crate: https://crates.io/crates/agent-client-protocol
- Documentation: https://docs.rs/agent-client-protocol
- GitHub: https://github.com/agentclientprotocol/agent-client-protocol
- Rust SDK: https://github.com/agentclientprotocol/rust-sdk

## Implementation Notes

The protocol is still under development but complete enough to build interesting user experiences. Agents that implement ACP work with any compatible editor, and editors that support ACP gain access to the entire ecosystem of ACP-compatible agents.

Note: Full support for remote agents is a work in progress.
