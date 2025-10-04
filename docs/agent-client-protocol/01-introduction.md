# Introduction to Agent Client Protocol

## What is ACP?

The Agent Client Protocol standardizes communication between code editors (IDEs, text-editors, etc.) and coding agents (programs that use generative AI to autonomously modify code).

The protocol is still under development, but it should be complete enough to build interesting user experiences using it.

## Why ACP?

AI coding agents and editors are tightly coupled but interoperability isn't the default. Each editor must build custom integrations for every agent they want to support, and agents must implement editor-specific APIs to reach users. This creates several problems:

- **Integration overhead**: Every new agent-editor combination requires custom work
- **Limited compatibility**: Agents work with only a subset of available editors
- **Developer lock-in**: Choosing an agent often means accepting their available interfaces

ACP solves this by providing a standardized protocol for agent-editor communication, similar to how the [Language Server Protocol (LSP)](https://microsoft.github.io/language-server-protocol/) standardized language server integration.

## Benefits

- **Agents that implement ACP** work with any compatible editor
- **Editors that support ACP** gain access to the entire ecosystem of ACP-compatible agents
- **Decoupling** allows both sides to innovate independently
- **Freedom** for developers to choose the best tools for their workflow

## Overview

ACP assumes that the user is primarily in their editor, and wants to reach out and use agents to assist them with specific tasks.

### Architecture

- **Agents run as sub-processes** of the code editor
- **Communication** uses JSON-RPC over stdio
- **JSON representations** re-use the types used in MCP where possible
- **Custom types** are included for useful agentic coding UX elements, like displaying diffs
- **Default format** for user-readable text is Markdown

## Key Components

### Agents

Agents are programs that use generative AI to autonomously modify code. They:
- Handle requests from clients
- Execute tasks using language models and tools
- Run as subprocesses of the Client
- Communicate via JSON-RPC 2.0 over stdio

### Clients

Clients provide the interface between users and agents. They are typically code editors (IDEs, text editors) but can also be other UIs for interacting with agents. Clients:
- Manage the environment
- Handle user interactions
- Control access to resources
- Display agent output and progress

### Sessions

Sessions represent independent conversation contexts with their own history and state, allowing multiple independent interactions with the same agent.

## Communication Model

The protocol follows the JSON-RPC 2.0 specification with two types of messages:

1. **Methods**: Request-response pairs that expect a result or error
2. **Notifications**: One-way messages that don't expect a response

## Protocol Requirements

- **All file paths** in the protocol MUST be absolute
- **Line numbers** are 1-based
- **Error handling** follows standard JSON-RPC 2.0 error handling:
  - Successful responses include a `result` field
  - Errors include an `error` object with `code` and `message`
  - Notifications never receive responses (success or error)

## Extensibility

The protocol provides built-in mechanisms for adding custom functionality while maintaining compatibility:

- Add custom data using `_meta` fields
- Create custom methods by prefixing their name with underscore (`_`)
- Advertise custom capabilities during initialization
