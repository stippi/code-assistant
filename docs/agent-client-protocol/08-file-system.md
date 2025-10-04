# File System

The filesystem methods allow Agents to read and write text files within the Client's environment. These methods enable Agents to access unsaved editor state and allow Clients to track file modifications made during agent execution.

## Checking Support

Before attempting to use filesystem methods, Agents **MUST** verify that the Client supports these capabilities by checking the Client Capabilities field in the `initialize` response:

```json
{
  "jsonrpc": "2.0",
  "id": 0,
  "result": {
    "protocolVersion": 1,
    "clientCapabilities": {
      "fs": {
        "readTextFile": true,
        "writeTextFile": true
      }
    }
  }
}
```

If `readTextFile` or `writeTextFile` is `false` or not present, the Agent **MUST NOT** attempt to call the corresponding filesystem method.

## Reading Files

The `fs/read_text_file` method allows Agents to read text file contents from the Client's filesystem, including unsaved changes in the editor.

### Request

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "fs/read_text_file",
  "params": {
    "sessionId": "sess_abc123def456",
    "path": "/home/user/project/src/main.py",
    "line": 10,
    "limit": 50
  }
}
```

### Request Parameters

- `sessionId` (required): The Session ID for this request
- `path` (required): Absolute path to the file to read
- `line` (optional): Line number to start reading from (1-based)
- `limit` (optional): Maximum number of lines to read

### Response

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "result": {
    "content": "def hello_world():\n    print('Hello, world!')\n"
  }
}
```

### Response Fields

- `content` (required): The text content of the file

## Writing Files

The `fs/write_text_file` method allows Agents to write or update text files in the Client's filesystem.

### Request

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "fs/write_text_file",
  "params": {
    "sessionId": "sess_abc123def456",
    "path": "/home/user/project/config.json",
    "content": "{\n  \"debug\": true,\n  \"version\": \"1.0.0\"\n}"
  }
}
```

### Request Parameters

- `sessionId` (required): The Session ID for this request
- `path` (required): Absolute path to the file to write. The Client **MUST** create the file if it doesn't exist
- `content` (required): The text content to write to the file

### Response

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "result": {}
}
```

The Client responds with an empty result on success.

## Use Cases

### Reading Unsaved Editor State

One key advantage of the filesystem methods is that they allow Agents to access the current editor state, including unsaved changes. This is particularly useful for:

- Analyzing code that hasn't been saved yet
- Understanding the current context without forcing saves
- Providing real-time feedback on work in progress

### Tracking Modifications

When Agents write files through the Client, the Client can:

- Track which files were modified during agent execution
- Show diffs in the UI
- Integrate with version control systems
- Provide undo/redo functionality

### Coordinated File Access

By going through the Client for file operations, Agents ensure:

- Proper file locking and coordination
- Consistent view of the filesystem
- Integration with editor features (syntax highlighting, etc.)
- Proper handling of encoding and line endings

## Best Practices

### For Agents

1. **Always check capabilities** before calling filesystem methods
2. **Use absolute paths** for all file operations
3. **Handle errors gracefully** when file operations fail
4. **Read only what you need** using line and limit parameters
5. **Consider file size** before reading entire files

### For Clients

1. **Respect working directory boundaries** defined in session setup
2. **Provide access to unsaved editor state** when reading files
3. **Track all file modifications** for UI display
4. **Create parent directories** as needed when writing files
5. **Handle encoding properly** for different file types

## Security Considerations

Clients should:

- Enforce appropriate access controls on file operations
- Validate that file paths are within expected boundaries
- Warn users about potentially dangerous operations
- Consider implementing approval workflows for file writes
- Log all file system operations for audit purposes
