---
title: Connect MCP servers
linkTitle: MCP servers
weight: 30
lede: Add tools from any Model Context Protocol server, configured for all projects or shipped with a project in .mcp.json. Turn servers on and off per session, and log in with OAuth when a server needs it.
frontends: [app, tui, acp]
---

## Add a server

{{< frontends >}}
{{< fe "app" >}}
In **Settings → MCP Servers**, click **New MCP Server**. Give it a **Name** and
pick the **Transport**:

- **Command (stdio)**: the command and its arguments, plus environment variables.
- **HTTP**: the URL, plus headers.

After saving, the card lists the tools the server offers. Switch individual
tools off there. **On in new sessions** decides whether new sessions start with
the server on. If a server needs a login, click **Authenticate** and finish
the login in your browser.
{{< /fe >}}
{{< fe "tui" >}}
Edit `mcp-servers.json` in the config folder (see below). For servers with an
OAuth login:

```bash
code-assistant mcp-login <server>     # opens the browser
code-assistant mcp-logout <server>
```
{{< /fe >}}
{{< fe "acp" >}}
The ACP agent uses the same `mcp-servers.json` and `.mcp.json` as the app.
Configure servers in the app or by hand.
{{< /fe >}}
{{< /frontends >}}

### `mcp-servers.json`

```json
{
  "servers": {
    "jira": {
      "command": "npx",
      "args": ["-y", "some-jira-server"],
      "env": { "JIRA_TOKEN": "${JIRA_TOKEN}" }
    },
    "docs": {
      "url": "https://example.com/mcp",
      "headers": { "Authorization": "Bearer ${DOCS_TOKEN}" },
      "off_in_new_sessions": true,
      "disabled_tools": ["delete_page"]
    }
  }
}
```

| Field | Meaning |
|---|---|
| `command`, `args`, `env` | Run the server as a child process over stdio |
| `url`, `headers` | Connect over HTTP (streamable transport) |
| `enabled` | `false` switches the server off everywhere |
| `enabled_tools` | Only offer these tools |
| `disabled_tools` | Never offer these tools (wins over `enabled_tools`) |
| `off_in_new_sessions` | New sessions start with the server off |

`${VAR}` in `env` and `headers` is replaced with the environment variable.
At the top level, `"auto_reauthorize": false` stops the automatic browser login
when an OAuth login expires (in Settings: **Log in again when a login
expires**).

Tools appear to the model as `mcp__<server>__<tool>`. Changes apply to the
next agent run, so you don't need to restart.

## Turn servers on and off per session

{{< selectors on="Tools" >}}<div class="menu"><div><b style="font-size:11px;color:var(--muted)">MCP SERVERS</b></div><div class="cur">jira ✓<small>12 tools</small></div><div>docs<small>Needs login · Log in</small></div><div><b style="font-size:11px;color:var(--muted)">BUILT-IN</b></div><div class="cur">Browser ✓</div></div>{{< /selectors >}}

In the app, the **Tools** menu in the selector row lists the MCP servers and
the [built-in tool groups](/docs/configure/tools/). Each has a switch. The
project remembers your last choice for its next sessions. The menu also
shows each server's state, with **Log in** for servers that need it and
**Restart server** for one that stopped.

## Project servers: `.mcp.json` {#project}

A project can include its servers in a `.mcp.json` at its root. The format is
the one Claude Code uses, so one committed file works for both:

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "."]
    },
    "issues": { "type": "http", "url": "https://example.com/mcp" }
  }
}
```

Project servers are merged with the global ones by name. If both define a
server with the same name, the project's definition is used.

### Trust

A `.mcp.json` comes with every clone of the repository, and loading it starts
the programs it names. So it is never loaded without asking. At the first run
in the project, and again after any change to the file, you are asked:

- **Load once**: for this run only.
- **Always (this project)**: remembered until the file changes.
- **Deny**: run with the global servers only.

## Resources

Servers can also offer **resources** (documents, records). The agent reads them
with its normal file tools, using the project name `:mcp:<server>` and the
resource URI as the path. The system prompt lists what each server offers.
