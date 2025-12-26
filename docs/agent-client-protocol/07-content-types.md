# Content Types

Content blocks represent displayable information that flows through the Agent Client Protocol. They provide a structured way to handle various types of user-facing content—whether it's text from language models, images for analysis, or embedded resources for context.

Content blocks appear in:
- User prompts sent via `session/prompt`
- Language model output streamed through `session/update` notifications
- Progress updates and results from tool calls

## Compatibility with MCP

The Agent Client Protocol uses the same `ContentBlock` structure as the [Model Context Protocol (MCP)](https://modelcontextprotocol.io/specification/2025-06-18/schema#contentblock).

This design choice enables Agents to seamlessly forward content from MCP tool outputs without transformation.

## Content Types

### Text Content

Plain text messages form the foundation of most interactions.

```json
{
  "type": "text",
  "text": "What's the weather like today?"
}
```

**Baseline Requirement**: All Agents **MUST** support text content blocks when included in prompts.

#### Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `text` | string | Yes | The text content to display |
| `annotations` | Annotations | No | Optional metadata about how the content should be used or displayed |

### Image Content

Images can be included for visual context or analysis.

```json
{
  "type": "image",
  "mimeType": "image/png",
  "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB..."
}
```

**Capability Requirement**: Requires the `image` prompt capability when included in prompts.

#### Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `data` | string | Yes | Base64-encoded image data |
| `mimeType` | string | Yes | The MIME type of the image (e.g., "image/png", "image/jpeg") |
| `uri` | string | No | Optional URI reference for the image source |
| `annotations` | Annotations | No | Optional metadata |

### Audio Content

Audio data for transcription or analysis.

```json
{
  "type": "audio",
  "mimeType": "audio/wav",
  "data": "UklGRiQAAABXQVZFZm10IBAAAAABAAEAQB8AAAB..."
}
```

**Capability Requirement**: Requires the `audio` prompt capability when included in prompts.

#### Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `data` | string | Yes | Base64-encoded audio data |
| `mimeType` | string | Yes | The MIME type of the audio (e.g., "audio/wav", "audio/mp3") |
| `annotations` | Annotations | No | Optional metadata |

### Embedded Resource

Complete resource contents embedded directly in the message.

```json
{
  "type": "resource",
  "resource": {
    "uri": "file:///home/user/script.py",
    "mimeType": "text/x-python",
    "text": "def hello():\n    print('Hello, world!')"
  }
}
```

This is the preferred way to include context in prompts, such as when using @-mentions to reference files or other resources.

By embedding the content directly in the request, Clients can include context from sources that the Agent may not have direct access to.

**Capability Requirement**: Requires the `embeddedContext` prompt capability when included in prompts.

#### Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `resource` | EmbeddedResourceResource | Yes | The embedded resource contents (can be text or blob) |
| `annotations` | Annotations | No | Optional metadata |

#### Text Resource

For text-based resources:

```json
{
  "uri": "file:///home/user/script.py",
  "mimeType": "text/x-python",
  "text": "def hello():\n    print('Hello, world!')"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `uri` | string | Yes | The URI identifying the resource |
| `text` | string | Yes | The text content of the resource |
| `mimeType` | string | No | MIME type of the text content |

#### Blob Resource

For binary resources:

```json
{
  "uri": "file:///home/user/image.png",
  "mimeType": "image/png",
  "blob": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB..."
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `uri` | string | Yes | The URI identifying the resource |
| `blob` | string | Yes | Base64-encoded binary data |
| `mimeType` | string | No | MIME type of the blob |

### Resource Link

References to resources that the Agent can access.

```json
{
  "type": "resource_link",
  "uri": "file:///home/user/document.pdf",
  "name": "document.pdf",
  "mimeType": "application/pdf",
  "size": 1024000
}
```

**Baseline Requirement**: All Agents **MUST** support resource links when included in prompts.

#### Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `uri` | string | Yes | The URI of the resource |
| `name` | string | Yes | A human-readable name for the resource |
| `mimeType` | string | No | The MIME type of the resource |
| `title` | string | No | Optional display title for the resource |
| `description` | string | No | Optional description of the resource contents |
| `size` | integer | No | Optional size of the resource in bytes |
| `annotations` | Annotations | No | Optional metadata |

## Annotations

Annotations provide optional metadata about how content should be used or displayed. See the [MCP specification](https://modelcontextprotocol.io/specification/2025-06-18/server/resources#annotations) for more details.

Common annotation fields:
- `audience`: Array indicating who should see this content
- `priority`: Number indicating importance (higher = more important)
- `lastModified`: Timestamp of last modification

## Content Usage Patterns

### In Prompts

When sending user prompts, Clients should:
1. Check the Agent's prompt capabilities from initialization
2. Only include content types that are supported
3. Prefer embedded resources over resource links when the Agent supports `embeddedContext`

### In Updates

When streaming responses, Agents can:
1. Send message chunks incrementally for streaming effect
2. Include rich content like images or diffs
3. Provide structured output via different content types

### In Tool Calls

Tool results can include:
1. Text output from operations
2. Diffs showing file changes
3. Terminal output for commands
4. Images or other rich media
