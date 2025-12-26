# Slash Commands

Agents can advertise a set of slash commands that users can invoke. These commands provide quick access to specific agent capabilities and workflows. Commands are run as part of regular prompt requests where the Client includes the command text in the prompt.

## Advertising Commands

After creating a session, the Agent **MAY** send a list of available commands via the `available_commands_update` session notification:

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "sess_abc123def456",
    "update": {
      "sessionUpdate": "available_commands_update",
      "availableCommands": [
        {
          "name": "web",
          "description": "Search the web for information",
          "input": {
            "hint": "query to search for"
          }
        },
        {
          "name": "test",
          "description": "Run tests for the current project"
        },
        {
          "name": "plan",
          "description": "Create a detailed implementation plan",
          "input": {
            "hint": "description of what to plan"
          }
        }
      ]
    }
  }
}
```

### Update Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `availableCommands` | AvailableCommand[] | Yes | The list of commands available in this session |

### AvailableCommand

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | Yes | The command name (e.g., "web", "test", "plan") |
| `description` | string | Yes | Human-readable description of what the command does |
| `input` | AvailableCommandInput | No | Optional input specification for the command |

### AvailableCommandInput

Currently supports unstructured text input:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `hint` | string | Yes | A hint to display when the input hasn't been provided yet |

## Dynamic Updates

The Agent can update the list of available commands at any time during a session by sending another `available_commands_update` notification. This allows commands to be:

- **Added** based on context (e.g., new commands become available after certain actions)
- **Removed** when no longer relevant
- **Modified** with updated descriptions

### Example: Adding Context-Specific Commands

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "sess_abc123def456",
    "update": {
      "sessionUpdate": "available_commands_update",
      "availableCommands": [
        {
          "name": "web",
          "description": "Search the web for information",
          "input": {
            "hint": "query to search for"
          }
        },
        {
          "name": "test",
          "description": "Run tests for the current project"
        },
        {
          "name": "deploy",
          "description": "Deploy changes to staging environment"
        }
      ]
    }
  }
}
```

## Running Commands

Commands are included as regular user messages in prompt requests:

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "session/prompt",
  "params": {
    "sessionId": "sess_abc123def456",
    "prompt": [
      {
        "type": "text",
        "text": "/web agent client protocol"
      }
    ]
  }
}
```

The Agent recognizes the command prefix and processes it accordingly. Commands may be accompanied by any other user message content types (images, audio, etc.) in the same prompt array.

### Command Syntax

Commands follow a simple syntax:
- Start with `/` followed by the command name
- Additional text after the command name is treated as input
- Example: `/web rust async programming` runs the "web" command with "rust async programming" as input

## Common Command Patterns

### Search Commands

```json
{
  "name": "web",
  "description": "Search the web for information",
  "input": {
    "hint": "search query"
  }
}
```

### Action Commands

```json
{
  "name": "test",
  "description": "Run tests for the current project"
}
```

### Planning Commands

```json
{
  "name": "plan",
  "description": "Create a detailed implementation plan",
  "input": {
    "hint": "what to plan"
  }
}
```

### Mode-Switching Commands

```json
{
  "name": "architect",
  "description": "Switch to architect mode for system design"
}
```

## Best Practices

### For Agents

1. **Use descriptive names** - Command names should be intuitive and memorable
2. **Provide clear descriptions** - Help users understand what each command does
3. **Update commands contextually** - Add/remove commands based on session state
4. **Handle invalid commands gracefully** - Provide helpful error messages
5. **Support command completion** - Consider the user experience when typing commands

### For Clients

1. **Show available commands** - Display commands in autocomplete or help UI
2. **Highlight command syntax** - Use different styling for command text
3. **Provide command hints** - Show the input hint when relevant
4. **Support tab completion** - Help users discover and complete commands
5. **Update UI when commands change** - Reflect dynamic command updates immediately

## Client UI Considerations

Clients should consider:

- **Autocomplete menus** - Show matching commands as users type `/`
- **Command palettes** - Allow users to browse all available commands
- **Inline hints** - Show expected input format after the command name
- **Keyboard shortcuts** - Allow quick access to common commands
- **Command history** - Remember recently used commands for quick access
