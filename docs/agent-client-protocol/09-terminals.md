# Terminals

The terminal methods allow Agents to execute shell commands within the Client's environment. These methods enable Agents to run build processes, execute scripts, and interact with command-line tools while providing real-time output streaming and process control.

## Checking Support

Before attempting to use terminal methods, Agents **MUST** verify that the Client supports this capability:

```json
{
  "jsonrpc": "2.0",
  "id": 0,
  "result": {
    "protocolVersion": 1,
    "clientCapabilities": {
      "terminal": true
    }
  }
}
```

If `terminal` is `false` or not present, the Agent **MUST NOT** attempt to call any terminal methods.

## Executing Commands

The `terminal/create` method starts a command in a new terminal:

### Request

```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "method": "terminal/create",
  "params": {
    "sessionId": "sess_abc123def456",
    "command": "npm",
    "args": ["test", "--coverage"],
    "env": [
      {
        "name": "NODE_ENV",
        "value": "test"
      }
    ],
    "cwd": "/home/user/project",
    "outputByteLimit": 1048576
  }
}
```

### Request Parameters

- `sessionId` (required): The Session ID for this request
- `command` (required): The command to execute
- `args` (optional): Array of command arguments
- `env` (optional): Environment variables for the command (array of `{name, value}` objects)
- `cwd` (optional): Working directory for the command (absolute path)
- `outputByteLimit` (optional): Maximum number of output bytes to retain

#### Output Byte Limit

Once exceeded, earlier output is truncated to stay within this limit. The Client **MUST** ensure truncation happens at a character boundary to maintain valid string output, even if this means the retained output is slightly less than the specified limit.

### Response

The Client returns a Terminal ID immediately without waiting for completion:

```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "result": {
    "terminalId": "term_xyz789"
  }
}
```

This allows the command to run in the background while the Agent performs other operations.

**Important**: The Agent **MUST** release the terminal using `terminal/release` when it's no longer needed.

## Embedding in Tool Calls

Terminals can be embedded directly in tool calls to provide real-time output to users:

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "sess_abc123def456",
    "update": {
      "sessionUpdate": "tool_call",
      "toolCallId": "call_002",
      "title": "Running tests",
      "kind": "execute",
      "status": "in_progress",
      "content": [
        {
          "type": "terminal",
          "terminalId": "term_xyz789"
        }
      ]
    }
  }
}
```

When a terminal is embedded in a tool call, the Client displays live output as it's generated and continues to display it even after the terminal is released.

## Getting Output

The `terminal/output` method retrieves the current terminal output without waiting for the command to complete:

### Request

```json
{
  "jsonrpc": "2.0",
  "id": 6,
  "method": "terminal/output",
  "params": {
    "sessionId": "sess_abc123def456",
    "terminalId": "term_xyz789"
  }
}
```

### Response

```json
{
  "jsonrpc": "2.0",
  "id": 6,
  "result": {
    "output": "Running tests...\n✓ All tests passed (42 total)\n",
    "truncated": false,
    "exitStatus": {
      "exitCode": 0,
      "signal": null
    }
  }
}
```

### Response Fields

- `output` (required): The terminal output captured so far
- `truncated` (required): Whether the output was truncated due to byte limits
- `exitStatus` (optional): Present only if the command has exited
  - `exitCode`: The process exit code (may be null)
  - `signal`: The signal that terminated the process (may be null)

## Waiting for Exit

The `terminal/wait_for_exit` method returns once the command completes:

### Request

```json
{
  "jsonrpc": "2.0",
  "id": 7,
  "method": "terminal/wait_for_exit",
  "params": {
    "sessionId": "sess_abc123def456",
    "terminalId": "term_xyz789"
  }
}
```

### Response

```json
{
  "jsonrpc": "2.0",
  "id": 7,
  "result": {
    "exitCode": 0,
    "signal": null
  }
}
```

### Response Fields

- `exitCode` (optional): The process exit code (may be null if terminated by signal)
- `signal` (optional): The signal that terminated the process (may be null if exited normally)

## Killing Commands

The `terminal/kill` method terminates a command without releasing the terminal:

### Request

```json
{
  "jsonrpc": "2.0",
  "id": 8,
  "method": "terminal/kill",
  "params": {
    "sessionId": "sess_abc123def456",
    "terminalId": "term_xyz789"
  }
}
```

### Response

```json
{
  "jsonrpc": "2.0",
  "id": 8,
  "result": {}
}
```

After killing a command, the terminal remains valid and can be used with:
- `terminal/output` to get the final output
- `terminal/wait_for_exit` to get the exit status

The Agent **MUST** still call `terminal/release` when done using it.

### Building a Timeout

Agents can implement command timeouts by combining terminal methods:

1. Create a terminal with `terminal/create`
2. Start a timer for the desired timeout duration
3. Concurrently wait for either the timer to expire or `terminal/wait_for_exit` to return
4. If the timer expires first:
   - Call `terminal/kill` to terminate the command
   - Call `terminal/output` to retrieve any final output
   - Include the output in the response to the model
5. Call `terminal/release` when done

## Releasing Terminals

The `terminal/release` method kills the command if still running and releases all resources:

### Request

```json
{
  "jsonrpc": "2.0",
  "id": 9,
  "method": "terminal/release",
  "params": {
    "sessionId": "sess_abc123def456",
    "terminalId": "term_xyz789"
  }
}
```

### Response

```json
{
  "jsonrpc": "2.0",
  "id": 9,
  "result": {}
}
```

After release, the terminal ID becomes invalid for all other `terminal/*` methods.

If the terminal was added to a tool call, the Client **SHOULD** continue to display its output after release.

## Terminal Lifecycle

```
1. Create terminal → terminalId returned immediately
2. (Optional) Embed in tool call for live display
3. Command runs in background
4. (Optional) Poll with terminal/output
5. (Optional) Wait with terminal/wait_for_exit
6. (Optional) Kill with terminal/kill if needed
7. Release with terminal/release (required)
```

## Use Cases

### Running Tests

```rust
// Create terminal and run tests
let terminal_id = client.create_terminal("npm", &["test"]).await?;

// Embed in tool call for live output
tool_call.add_terminal(terminal_id);

// Wait for completion
let exit_status = client.wait_for_exit(terminal_id).await?;

// Check result
if exit_status.exit_code == Some(0) {
    // Tests passed
}

// Clean up
client.release_terminal(terminal_id).await?;
```

### Running with Timeout

```rust
// Create terminal
let terminal_id = client.create_terminal("long-running-cmd", &[]).await?;

// Wait with timeout
let result = tokio::time::timeout(
    Duration::from_secs(30),
    client.wait_for_exit(terminal_id)
).await;

match result {
    Ok(exit_status) => {
        // Completed within timeout
    }
    Err(_) => {
        // Timeout - kill and get output
        client.kill_terminal(terminal_id).await?;
        let output = client.get_terminal_output(terminal_id).await?;
        // Process output...
    }
}

// Always release
client.release_terminal(terminal_id).await?;
```

### Background Process

```rust
// Start a server in background
let terminal_id = client.create_terminal("npm", &["run", "dev"]).await?;

// Do other work...
// Periodically check if still running
let output = client.get_terminal_output(terminal_id).await?;
if output.exit_status.is_some() {
    // Server crashed
}

// When done, kill and release
client.kill_terminal(terminal_id).await?;
client.release_terminal(terminal_id).await?;
```

## Best Practices

### For Agents

1. **Always release terminals** when done to avoid resource leaks
2. **Set output byte limits** to avoid memory issues with verbose commands
3. **Embed in tool calls** when users should see live output
4. **Use timeouts** for commands that might hang
5. **Check exit codes** to detect failures

### For Clients

1. **Display live output** for embedded terminals
2. **Truncate at character boundaries** when applying byte limits
3. **Kill processes on release** if still running
4. **Continue displaying output** after release if embedded in tool call
5. **Provide terminal UI** that shows command status and output

## Security Considerations

Clients should:

- Validate commands before execution
- Consider sandboxing or containerization
- Limit resource usage (CPU, memory, disk I/O)
- Implement timeouts to prevent runaway processes
- Log all terminal commands for audit purposes
- Consider requiring user approval for sensitive commands
