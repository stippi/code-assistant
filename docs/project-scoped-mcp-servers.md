# Project-scoped MCP servers

In addition to the globally configured MCP servers
(`<config_dir>/mcp-servers.json`, see [mcp-client-mode.md](mcp-client-mode.md)),
a project can ship its own servers in a **`.mcp.json` at the project root**.
The file format follows the convention Claude Code established, so one
committed file serves both tools.

## The `.mcp.json` format

```jsonc
{
  "mcpServers": {
    "filesystem": {                         // stdio: "type" may be omitted
      "type": "stdio",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "."],
      "env": { "NODE_ENV": "production" }
    },
    "internal-api": {
      "type": "http",                       // "sse" / "streamable-http" alias http
      "url": "https://api.example.com/mcp",
      "headers": { "Authorization": "Bearer ${API_TOKEN}" }
    }
  }
}
```

- Top-level key is **`mcpServers`** (Claude Code's key; the global
  `mcp-servers.json` keeps its own `servers` key — the two formats are parsed
  by separate code, `mcp_client::parse_local_mcp_json` for this one).
- Transport is chosen by the explicit `type` field; without one, a `url`
  implies HTTP and a `command` implies stdio. An explicit `"type": "stdio"`
  with a stray `url` is an error (missing `command`), not silently HTTP.
- `${VAR}` references in `env` and `headers` values are substituted from the
  environment at load time, so the committed file never carries secrets. A
  server whose variables cannot be resolved is skipped with a log warning
  (it could not connect anyway); the other servers still load. Note that
  config fingerprints read the files raw, so exporting a previously missing
  variable takes effect on the next rebuild (a config edit or restart), not
  by itself.
- Project entries carry no `enabled`/`enabled_tools`/`disabled_tools`
  metadata; every listed server is on (per-session toggles below).

## Merge semantics

The effective server set for a session is the global config overlaid by the
project's `.mcp.json`, **merged at the server level by name — project wins**.
A colliding name takes the project file's entire server definition; the
global definition of that name contributes nothing, so there is never a
tool-level collision (tool names `mcp__<server>__<tool>` derive from the
surviving server only).

## Trust

A `.mcp.json` arrives with a cloned repository, and loading it launches the
processes it names — so an untrusted project file is never loaded silently.
At the first agent run in a project with an untrusted (or edited)
`.mcp.json`, the user is prompted with the server names and three choices:

- **Load once** — this run only; ask again next time.
- **Always (this project)** — persists in `<config_dir>/mcp-trust.json`,
  keyed by canonicalized project path to an md5 fingerprint of the file's
  contents. Any later edit to the file changes the fingerprint and
  re-prompts.
- **Deny** — the run proceeds with global servers only.

The prompt travels the normal permission-mediator path, so it works in the
GPUI, terminal, and ACP frontends alike (the option list is defined once, in
`session::permissions::permission_options_for`). A frontend that cannot ask
skips the project's servers with a log warning. Revoking a persisted trust
currently means editing `mcp-trust.json` by hand — an untrust UI is an open
follow-up.

## Per-session server toggles

Each session can deactivate individual servers (global or project-local) via
the MCP selector in the input bar; the set persists in the session config
(`disabled_mcp_servers`). Deactivation filters the **tool set** offered to
and callable by that session's agent (via the per-server
`scope:mcp-<server>` capability tag) — it does not prevent the server
process from starting, since registries and their connections are shared
across sessions.

## Registries and connection lifecycle

Because a project file makes the tool registry project-dependent, registries
are built **per project** and cached by `tools::ConfigToolRegistry`:

- The cache is keyed by the directory whose `.mcp.json` is included (`None`
  for the global-only registry; sessions without a trusted project file all
  share that one). Sessions running in different projects at the same time
  each keep their own registry — starting a run in one project does not
  invalidate, or relaunch the servers of, another.
- A cache entry is validated by a fingerprint of `tools.json`,
  `mcp-servers.json`, and the project's `.mcp.json` (all read raw, so a
  changed environment alone does not rebuild). The provider is consulted at
  the start of every agent run; an unchanged configuration returns the same
  `Arc`. A running agent keeps the registry it started with.
- **Lifetimes are ownership-driven.** The cache and the MCP connection pool
  hold only `Weak` references. A registry stays alive as long as a session
  instance or in-flight run holds it; each registry holds its connections
  via `Arc` inside its tools. Connections are pooled by server identity
  (name + transport config), so servers shared between registries —
  typically the global set — run once, and a server's child process
  terminates when the last registry using it is dropped. In practice: a
  project's servers run while a session that uses them is open, and nothing
  is ever shut down under a live run.

## Rendering is registry-independent

Recorded MCP tool executions (`mcp__…` names) deserialize from their
self-describing persisted JSON (`mcp_client::deserialize_mcp_output`), never
through a registry lookup (`tools::mcp::deserialize_tool_execution`). A
session therefore renders identically wherever it is viewed — under another
project's registry, or in a second code-assistant instance that never
launched (or never trusted) the servers that produced the output. Only
native tools still require a registry entry to render.

## Known limitations

- No UI to revoke a persisted trust (edit `mcp-trust.json` by hand).
- Per-session deactivation hides a server's tools but does not prevent its
  process from launching.
- The settings page edits the global config only; project files are edited
  in the repository.
