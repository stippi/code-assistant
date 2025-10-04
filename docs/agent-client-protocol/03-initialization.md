# Initialization

The Initialization phase allows Clients and Agents to negotiate protocol versions, capabilities, and authentication methods.

Before a Session can be created, Clients **MUST** initialize the connection by calling the `initialize` method.

## Initialize Request

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
    }
  }
}
```

### Request Parameters

- `protocolVersion` (required): The latest protocol version supported by the Client
- `clientCapabilities` (optional): Capabilities supported by the client

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
      "mcpCapabilities": {
        "http": true,
        "sse": true
      }
    },
    "authMethods": []
  }
}
```

### Response Fields

- `protocolVersion` (required): The negotiated protocol version
- `agentCapabilities` (optional): Capabilities supported by the agent
- `authMethods` (optional): Authentication methods supported by the agent

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

```json
{
  "fs": {
    "readTextFile": boolean,  // fs/read_text_file method is available
    "writeTextFile": boolean  // fs/write_text_file method is available
  }
}
```

#### Terminal

```json
{
  "terminal": boolean  // All terminal/* methods are available
}
```

### Agent Capabilities

The Agent **SHOULD** specify whether it supports the following capabilities:

#### Load Session

```json
{
  "loadSession": boolean  // session/load method is available (default: false)
}
```

#### Prompt Capabilities

Indicates which content types beyond the baseline (text and resource links) the agent can process.

As a baseline, all Agents **MUST** support `ContentBlock::Text` and `ContentBlock::ResourceLink` in `session/prompt` requests.

```json
{
  "promptCapabilities": {
    "image": boolean,           // ContentBlock::Image supported (default: false)
    "audio": boolean,           // ContentBlock::Audio supported (default: false)
    "embeddedContext": boolean  // ContentBlock::Resource supported (default: false)
  }
}
```

#### MCP Capabilities

```json
{
  "mcpCapabilities": {
    "http": boolean,  // Agent supports connecting to MCP servers over HTTP (default: false)
    "sse": boolean    // Agent supports connecting to MCP servers over SSE (default: false)
  }
}
```

Note: The SSE transport has been deprecated by the MCP spec.

## Custom Capabilities

Implementations can advertise custom capabilities using the `_meta` field to indicate support for protocol extensions.

## Authentication

If the Agent requires authentication, it will advertise available `authMethods` in the initialize response. The Client must then call the `authenticate` method before creating sessions.

## Next Steps

Once the connection is initialized, you're ready to create a session and begin the conversation with the Agent.
