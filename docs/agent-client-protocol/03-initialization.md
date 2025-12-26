# Initialization

The Initialization phase allows Clients and Agents to negotiate protocol versions, capabilities, and authentication methods.

Before a Session can be created, Clients **MUST** initialize the connection by calling the `initialize` method.

## Initialize Request

Clients **MUST** initialize the connection with:
- The latest protocol version supported
- The capabilities supported

They **SHOULD** also provide a name and version to the Agent.

```json
{
  "jsonrpc": "2.0",
  "id": 0,
  "method": "initialize",
  "params": {
    "protocolVersion": 1,
    "clientCapabilities": {
      "fs": {
        "readTextFile": true,
        "writeTextFile": true
      },
      "terminal": true
    },
    "clientInfo": {
      "name": "my-client",
      "title": "My Client",
      "version": "1.0.0"
    }
  }
}
```

### Request Parameters

- `protocolVersion` (required): The latest protocol version supported by the Client
- `clientCapabilities` (optional): Capabilities supported by the client
- `clientInfo` (optional): Information about the Client name and version (will be required in future versions)

## Initialize Response

The Agent **MUST** respond with the chosen protocol version and the capabilities it supports:

```json
{
  "jsonrpc": "2.0",
  "id": 0,
  "result": {
    "protocolVersion": 1,
    "agentCapabilities": {
      "loadSession": true,
      "promptCapabilities": {
        "image": true,
        "audio": true,
        "embeddedContext": true
      },
      "mcp": {
        "http": true,
        "sse": true
      }
    },
    "agentInfo": {
      "name": "my-agent",
      "title": "My Agent",
      "version": "1.0.0"
    },
    "authMethods": []
  }
}
```

### Response Fields

- `protocolVersion` (required): The negotiated protocol version
- `agentCapabilities` (optional): Capabilities supported by the agent
- `agentInfo` (optional): Information about the Agent name and version (will be required in future versions)
- `authMethods` (optional): Authentication methods supported by the agent

## Implementation Information

Both Clients and Agents **SHOULD** provide information about their implementation in the `clientInfo` and `agentInfo` fields respectively:

- `name` (required): Intended for programmatic or logical use, but can be used as a display name fallback if title isn't present
- `title` (optional): Intended for UI and end-user contexts — optimized to be human-readable and easily understood
- `version` (required): Version of the implementation. Can be displayed to the user or used for debugging or metrics purposes

> Note: In future versions of the protocol, this information will be required.

## Protocol Version

The protocol versions are a single integer that identifies a **MAJOR** protocol version. This version is only incremented when breaking changes are introduced.

Clients and Agents **MUST** agree on a protocol version and act according to its specification.

### Version Negotiation

1. The `initialize` request **MUST** include the latest protocol version the Client supports
2. If the Agent supports the requested version, it **MUST** respond with the same version
3. Otherwise, the Agent **MUST** respond with the latest version it supports
4. If the Client does not support the version specified by the Agent, the Client **SHOULD** close the connection and inform the user

## Capabilities

Capabilities describe features supported by the Client and the Agent.

- All capabilities are **OPTIONAL**
- Clients and Agents **SHOULD** support all possible combinations of their peer's capabilities
- Introduction of new capabilities is not considered a breaking change
- Clients and Agents **MUST** treat all capabilities omitted as **UNSUPPORTED**

### Client Capabilities

The Client **SHOULD** specify whether it supports the following capabilities:

#### File System

| Field | Type | Description |
|-------|------|-------------|
| `readTextFile` | boolean | The `fs/read_text_file` method is available |
| `writeTextFile` | boolean | The `fs/write_text_file` method is available |

#### Terminal

| Field | Type | Description |
|-------|------|-------------|
| `terminal` | boolean | All `terminal/*` methods are available |

### Agent Capabilities

The Agent **SHOULD** specify whether it supports the following capabilities:

#### Load Session

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `loadSession` | boolean | false | The `session/load` method is available |

#### Prompt Capabilities

Indicates which content types beyond the baseline (text and resource links) the agent can process.

As a baseline, all Agents **MUST** support `ContentBlock::Text` and `ContentBlock::ResourceLink` in `session/prompt` requests.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `image` | boolean | false | `ContentBlock::Image` supported |
| `audio` | boolean | false | `ContentBlock::Audio` supported |
| `embeddedContext` | boolean | false | `ContentBlock::Resource` supported |

#### MCP Capabilities

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `http` | boolean | false | Agent supports connecting to MCP servers over HTTP |
| `sse` | boolean | false | Agent supports connecting to MCP servers over SSE (deprecated) |

> Note: The SSE transport has been deprecated by the MCP spec.

#### Session Capabilities

As a baseline, all Agents **MUST** support `session/new`, `session/prompt`, `session/cancel`, and `session/update`.

Optionally, they **MAY** support other session methods and notifications by specifying additional capabilities.

> Note: `session/load` is still handled by the top-level `loadSession` capability. This will be unified in future versions of the protocol.

## Custom Capabilities

Implementations can advertise custom capabilities using the `_meta` field to indicate support for protocol extensions.

## Authentication

If the Agent requires authentication, it will advertise available `authMethods` in the initialize response. The Client must then call the `authenticate` method before creating sessions.

## Next Steps

Once the connection is initialized, you're ready to create a session and begin the conversation with the Agent.
