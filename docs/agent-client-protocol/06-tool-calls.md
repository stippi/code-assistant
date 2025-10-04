# Tool Calls

Tool calls represent actions that language models request Agents to perform during a prompt turn. When an LLM determines it needs to interact with external systems—like reading files, running code, or fetching data—it generates tool calls that the Agent executes on its behalf.

Agents report tool calls through `session/update` notifications, allowing Clients to display real-time progress and results to users.

While Agents handle the actual execution, they may leverage Client capabilities like permission requests or file system access to provide a richer, more integrated experience.

## Creating

When the language model requests a tool invocation, the Agent **SHOULD** report it to the Client:

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "sess_abc123def456",
    "update": {
      "sessionUpdate": "tool_call",
      "toolCallId": "call_001",
      "title": "Reading configuration file",
      "kind": "read",
      "status": "pending"
    }
  }
}
```

### Tool Call Fields

- `toolCallId` (required): A unique identifier for this tool call within the session
- `title` (required): A human-readable title describing what the tool is doing
- `kind` (optional): The category of tool being invoked (defaults to `other`)
- `status` (optional): The current execution status (defaults to `pending`)
- `content` (optional): Content produced by the tool call
- `locations` (optional): File locations affected by this tool call
- `rawInput` (optional): The raw input parameters sent to the tool
- `rawOutput` (optional): The raw output returned by the tool

### Tool Kinds

Tool kinds help Clients choose appropriate icons and optimize how they display tool execution progress:

- `read`: Reading files or data
- `edit`: Modifying files or content
- `delete`: Removing files or data
- `move`: Moving or renaming files
- `search`: Searching for information
- `execute`: Running commands or code
- `think`: Internal reasoning or planning
- `fetch`: Retrieving external data
- `switch_mode`: Switching the current session mode
- `other`: Other tool types (default)

## Updating

As tools execute, Agents send updates to report progress and results using `tool_call_update`:

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "sess_abc123def456",
    "update": {
      "sessionUpdate": "tool_call_update",
      "toolCallId": "call_001",
      "status": "in_progress",
      "content": [
        {
          "type": "content",
          "content": {
            "type": "text",
            "text": "Found 3 configuration files..."
          }
        }
      ]
    }
  }
}
```

All fields except `toolCallId` are optional in updates. Only the fields being changed need to be included.

## Status

Tool calls progress through different statuses during their lifecycle:

- `pending`: The tool call hasn't started running yet because the input is either streaming or awaiting approval
- `in_progress`: The tool call is currently running
- `completed`: The tool call completed successfully
- `failed`: The tool call failed with an error

## Content

Tool calls can produce different types of content:

### Regular Content

Standard content blocks like text, images, or resources:

```json
{
  "type": "content",
  "content": {
    "type": "text",
    "text": "Analysis complete. Found 3 issues."
  }
}
```

### Diffs

File modifications shown as diffs:

```json
{
  "type": "diff",
  "path": "/home/user/project/src/config.json",
  "oldText": "{\n  \"debug\": false\n}",
  "newText": "{\n  \"debug\": true\n}"
}
```

Fields:
- `path` (required): The absolute file path being modified
- `oldText` (optional): The original content (null for new files)
- `newText` (required): The new content after modification

### Terminals

Live terminal output from command execution:

```json
{
  "type": "terminal",
  "terminalId": "term_xyz789"
}
```

Fields:
- `terminalId` (required): The ID of a terminal created with `terminal/create`

When a terminal is embedded in a tool call, the Client displays live output as it's generated and continues to display it even after the terminal is released.

## Requesting Permission

The Agent **MAY** request permission from the user before executing a tool call:

```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "method": "session/request_permission",
  "params": {
    "sessionId": "sess_abc123def456",
    "toolCall": {
      "toolCallId": "call_001"
    },
    "options": [
      {
        "optionId": "allow-once",
        "name": "Allow once",
        "kind": "allow_once"
      },
      {
        "optionId": "reject-once",
        "name": "Reject",
        "kind": "reject_once"
      }
    ]
  }
}
```

### Request Parameters

- `sessionId` (required): The session ID for this request
- `toolCall` (required): The tool call update containing details about the operation
- `options` (required): Available permission options for the user to choose from

### Permission Response

The Client responds with the user's decision:

```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "result": {
    "outcome": {
      "outcome": "selected",
      "optionId": "allow-once"
    }
  }
}
```

Clients **MAY** automatically allow or reject permission requests according to user settings.

### Cancellation Outcome

If the current prompt turn gets cancelled, the Client **MUST** respond with the `"cancelled"` outcome:

```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "result": {
    "outcome": {
      "outcome": "cancelled"
    }
  }
}
```

### Permission Options

Each permission option provided to the Client contains:

- `optionId` (required): Unique identifier for this option
- `name` (required): Human-readable label to display to the user
- `kind` (required): A hint to help Clients choose appropriate icons and UI treatment

#### Permission Option Kinds

- `allow_once`: Allow this operation only this time
- `allow_always`: Allow this operation and remember the choice
- `reject_once`: Reject this operation only this time
- `reject_always`: Reject this operation and remember the choice

## Following the Agent

Tool calls can report file locations they're working with, enabling Clients to implement "follow-along" features that track which files the Agent is accessing or modifying in real-time.

```json
{
  "path": "/home/user/project/src/main.py",
  "line": 42
}
```

### Location Fields

- `path` (required): The absolute file path being accessed or modified
- `line` (optional): Line number within the file
