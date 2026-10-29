---
title: MCP server mode
weight: 30
lede: Expose Code Assistant's file, search and command tools to Claude Desktop or any other MCP client. The client's model drives the tools, and your projects are the workspace.
frontends: []
---

In this mode Code Assistant has no agent loop of its own. It is a headless
[Model Context Protocol](https://modelcontextprotocol.io) server over stdio
that gives another client's model its tools for your configured projects.

## Claude Desktop

In Claude Desktop, open **Settings → Developer → Edit Config** and add:

```json
{
  "mcpServers": {
    "code-assistant": {
      "command": "/path/to/code-assistant",
      "args": ["server"],
      "env": { "SHELL": "/bin/zsh" }
    }
  }
}
```

Set `SHELL` so commands run in your usual shell. Restart Claude Desktop.

## Tools offered

| Group | Tools |
|---|---|
| Projects | `list_projects` |
| Files | `list_files`, `glob_files`, `read_files`, `search_files`, `write_file`, `edit`, `delete_files` |
| Documents and images | `view_documents`, `view_images` |
| Commands | `execute_command`, `write_stdin` |
| Web | `web_search`, `web_fetch`, `perplexity_ask` (with an API key) |
| Session history | `search_sessions`, `get_session_content` |
| Skills | `list_skills`, `read_skill` |

Every tool takes a `project` parameter naming one of the projects in
[`projects.json`](/docs/configure/projects/).

{{< callout warn "No one to ask" >}}
There is nobody to answer permission prompts in this mode. Keep the
permission tier at `bypass-all`, and use the client's own approval settings
to control tool use.
{{< /callout >}}
