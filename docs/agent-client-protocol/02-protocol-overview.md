# Protocol Overview

The Agent Client Protocol allows Agents and Clients to communicate by exposing methods that each side can call and sending notifications to inform each other of events.

## Communication Model

The protocol follows the [JSON-RPC 2.0](https://www.jsonrpc.org/specification) specification with two types of messages:

- **Methods**: Request-response pairs that expect a result or error
- **Notifications**: One-way messages that don't expect a response

## Message Flow

A typical flow follows this pattern:

### 1. Initialization Phase

- Client → Agent: `initialize` to establish connection
- Client → Agent: `authenticate` if required by the Agent

### 2. Session Setup

Either:
- Client → Agent: `session/new` to create a new session
- Client → Agent: `session/load` to resume an existing session if supported

### 3. Prompt Turn

- Client → Agent: `session/prompt` to send user message
- Agent → Client: `session/update` notifications for progress updates
- Agent → Client: File operations or permission requests as needed
- Client → Agent: `session/cancel` to interrupt processing if needed
- Turn ends and the Agent sends the `session/prompt` response with a stop reason

## Agent Methods

Methods that Agents must or may implement.

### Baseline Methods (Required)

#### `initialize`
Negotiate versions and exchange capabilities.

#### `authenticate`
Authenticate with the Agent (if required).

#### `session/new`
Create a new conversation session.

#### `session/prompt`
Send user prompts to the Agent.

### Optional Methods

#### `session/load`
Load an existing session (requires `loadSession` capability).

#### `session/set_mode`
Switch between agent operating modes.

### Agent Notifications

#### `session/cancel`
Cancel ongoing operations (no response expected).

## Client Methods

Methods that Clients must or may implement.

### Baseline Methods (Required)

#### `session/request_permission`
Request user authorization for tool calls.

### Optional Methods

#### `fs/read_text_file`
Read file contents (requires `fs.readTextFile` capability).

#### `fs/write_text_file`
Write file contents (requires `fs.writeTextFile` capability).

#### `terminal/create`
Create a new terminal (requires `terminal` capability).

#### `terminal/output`
Get terminal output and exit status (requires `terminal` capability).

#### `terminal/release`
Release a terminal (requires `terminal` capability).

#### `terminal/wait_for_exit`
Wait for terminal command to exit (requires `terminal` capability).

#### `terminal/kill`
Kill terminal command without releasing (requires `terminal` capability).

### Client Notifications

#### `session/update`
Send session updates to inform the Client of changes (no response expected). This includes:
- Message chunks (agent, user, thought)
- Tool calls and updates
- Plans
- Available commands updates
- Mode changes

## Argument Requirements

- All file paths in the protocol **MUST** be absolute
- Line numbers are 1-based

## Error Handling

All methods follow standard JSON-RPC 2.0 error handling:

- Successful responses include a `result` field
- Errors include an `error` object with `code` and `message`
- Notifications never receive responses (success or error)

## Extensibility

The protocol provides built-in mechanisms for adding custom functionality while maintaining compatibility:

- Add custom data using `_meta` fields
- Create custom methods by prefixing their name with underscore (`_`)
- Advertise custom capabilities during initialization

## Transport

- **Protocol**: JSON-RPC 2.0
- **Transport**: stdio (standard input/output)
- Agents run as subprocesses of the Client
- All communication is via JSON messages over stdio
