# Agent Plan

Plans are execution strategies for complex tasks that require multiple steps. Agents may share plans with Clients through `session/update` notifications, providing real-time visibility into their thinking and progress.

## Creating Plans

When the language model creates an execution plan, the Agent **SHOULD** report it to the Client:

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "sess_abc123def456",
    "update": {
      "sessionUpdate": "plan",
      "entries": [
        {
          "content": "Analyze the existing codebase structure",
          "priority": "high",
          "status": "pending"
        },
        {
          "content": "Identify components that need refactoring",
          "priority": "high",
          "status": "pending"
        },
        {
          "content": "Create unit tests for critical functions",
          "priority": "medium",
          "status": "pending"
        }
      ]
    }
  }
}
```

### Plan Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `entries` | PlanEntry[] | Yes | An array of plan entries representing the tasks to be accomplished |

## Plan Entries

Each plan entry represents a specific task or goal within the overall execution strategy:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `content` | string | Yes | A human-readable description of what this task aims to accomplish |
| `priority` | PlanEntryPriority | Yes | The relative importance of this task |
| `status` | PlanEntryStatus | Yes | The current execution status of this task |

### Priority Levels

| Priority | Description |
|----------|-------------|
| `high` | High priority task - critical to the overall goal |
| `medium` | Medium priority task - important but not critical |
| `low` | Low priority task - nice to have but not essential |

### Status Values

| Status | Description |
|--------|-------------|
| `pending` | The task has not started yet |
| `in_progress` | The task is currently being worked on |
| `completed` | The task has been successfully completed |

## Updating Plans

As the Agent progresses through the plan, it **SHOULD** report updates by sending more `session/update` notifications with the same structure.

The Agent **MUST** send a complete list of all plan entries in each update and their current status. The Client **MUST** replace the current plan completely.

### Example Update

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "sess_abc123def456",
    "update": {
      "sessionUpdate": "plan",
      "entries": [
        {
          "content": "Analyze the existing codebase structure",
          "priority": "high",
          "status": "completed"
        },
        {
          "content": "Identify components that need refactoring",
          "priority": "high",
          "status": "in_progress"
        },
        {
          "content": "Create unit tests for critical functions",
          "priority": "medium",
          "status": "pending"
        }
      ]
    }
  }
}
```

## Dynamic Planning

Plans can evolve during execution. The Agent **MAY** add, remove, or modify plan entries as it discovers new requirements or completes tasks, allowing it to adapt based on what it learns.

### Adding New Tasks

As the agent works, it may discover additional tasks that weren't in the original plan:

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "sess_abc123def456",
    "update": {
      "sessionUpdate": "plan",
      "entries": [
        {
          "content": "Analyze the existing codebase structure",
          "priority": "high",
          "status": "completed"
        },
        {
          "content": "Identify components that need refactoring",
          "priority": "high",
          "status": "completed"
        },
        {
          "content": "Fix circular dependency in auth module",
          "priority": "high",
          "status": "in_progress"
        },
        {
          "content": "Create unit tests for critical functions",
          "priority": "medium",
          "status": "pending"
        }
      ]
    }
  }
}
```

## Client Display

Clients **SHOULD** display plans to users in a way that:

- Shows the overall progress through the plan
- Indicates which task is currently being worked on
- Allows users to see the agent's thinking and strategy
- Updates in real-time as the agent sends new plan updates

## Best Practices

### For Agents

1. **Keep entries concise** - Each entry should describe a clear, actionable task
2. **Update frequently** - Send plan updates as status changes occur
3. **Use appropriate priorities** - Mark critical path items as high priority
4. **Be flexible** - Update the plan when new information is discovered

### For Clients

1. **Show progress visually** - Use progress bars, checkmarks, or status icons
2. **Highlight current task** - Make it clear what the agent is currently working on
3. **Handle plan changes gracefully** - Animate or indicate when tasks are added/removed
4. **Provide context** - Help users understand why the plan is structured this way
