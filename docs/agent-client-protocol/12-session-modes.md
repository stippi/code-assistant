# Session Modes

Agents can provide a set of modes they can operate in. Modes often affect the system prompts used, the availability of tools, and whether they request permission before running.

## Initial State

During Session Setup the Agent **MAY** return a list of modes it can operate in and the currently active mode:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "sessionId": "sess_abc123def456",
    "modes": {
      "currentModeId": "ask",
      "availableModes": [
        {
          "id": "ask",
          "name": "Ask",
          "description": "Request permission before making any changes"
        },
        {
          "id": "architect",
          "name": "Architect",
          "description": "Design and plan software systems without implementation"
        },
        {
          "id": "code",
          "name": "Code",
          "description": "Write and modify code with full tool access"
        }
      ]
    }
  }
}
```

### SessionModeState

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `currentModeId` | SessionModeId | Yes | The ID of the mode that is currently active |
| `availableModes` | SessionMode[] | Yes | The set of modes that the Agent can operate in |

### SessionMode

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | SessionModeId | Yes | Unique identifier for this mode |
| `name` | string | Yes | Human-readable name of the mode |
| `description` | string | No | Optional description providing more details about what this mode does |

## Setting the Current Mode

The current mode can be changed at any point during a session, whether the Agent is idle or generating a response.

### From the Client

Typically, Clients display the available modes to the user and allow them to change the current one, which they can do by calling the `session/set_mode` method.

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "session/set_mode",
  "params": {
    "sessionId": "sess_abc123def456",
    "modeId": "code"
  }
}
```

#### Request Parameters

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `sessionId` | SessionId | Yes | The ID of the session to set the mode for |
| `modeId` | SessionModeId | Yes | The ID of the mode to switch to. Must be one of the modes listed in `availableModes` |

### From the Agent

The Agent can also change its own mode and let the Client know by sending the `current_mode_update` session notification:

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "sess_abc123def456",
    "update": {
      "sessionUpdate": "current_mode_update",
      "modeId": "code"
    }
  }
}
```

### Exiting Plan Modes

A common case where an Agent might switch modes is from within a special "exit mode" tool that can be provided to the language model during plan/architect modes. The language model can call this tool when it determines it's ready to start implementing a solution.

This "switch mode" tool will usually request permission before running, which it can do just like any other tool:

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "session/request_permission",
  "params": {
    "sessionId": "sess_abc123def456",
    "toolCall": {
      "toolCallId": "call_switch_mode_001",
      "title": "Ready for implementation",
      "kind": "switch_mode",
      "status": "pending",
      "content": [
        {
          "type": "content",
          "content": {
            "type": "text",
            "text": "## Implementation Plan..."
          }
        }
      ]
    },
    "options": [
      {
        "optionId": "code",
        "name": "Yes, and auto-accept all actions",
        "kind": "allow_always"
      },
      {
        "optionId": "ask",
        "name": "Yes, and manually accept actions",
        "kind": "allow_once"
      },
      {
        "optionId": "reject",
        "name": "No, stay in architect mode",
        "kind": "reject_once"
      }
    ]
  }
}
```

When an option is chosen, the tool runs, setting the mode and sending the `current_mode_update` notification mentioned above.

## Common Mode Patterns

### Ask Mode

- **Purpose**: Conservative mode that requests user approval before making changes
- **Tools**: All tools available, but each requires explicit permission
- **Use case**: When users want full control over agent actions

### Architect Mode

- **Purpose**: Planning and design without code execution
- **Tools**: Read-only tools, no file modifications
- **Use case**: Initial exploration and system design

### Code Mode

- **Purpose**: Full implementation with minimal interruptions
- **Tools**: All tools available with automatic approval for safe operations
- **Use case**: When users trust the agent to make changes

## Best Practices

### For Agents

1. **Respect mode boundaries** - Only use tools appropriate for the current mode
2. **Provide clear mode descriptions** - Help users understand what each mode does
3. **Support mode transitions** - Allow smooth switching between modes
4. **Default to safer modes** - Start in ask mode unless user preferences indicate otherwise

### For Clients

1. **Display current mode prominently** - Users should always know what mode is active
2. **Make mode switching easy** - Provide a clear UI for changing modes
3. **Show mode restrictions** - Indicate what capabilities are available in each mode
4. **Confirm mode changes** - Especially when switching to less restrictive modes
