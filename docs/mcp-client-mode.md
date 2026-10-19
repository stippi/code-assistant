# MCP client mode — design note

Status: **implemented** (branch `feature/mcp-client-mode`, 2026-07-05) — see
"Implementation notes" at the end for what landed and what deviates from
this note. Originally written for the pal handoff; pal (the first
downstream consumer of the agent stack) wants MCP servers as its
procurement channel for integrations — email, calendars, Jira-like
external systems — without bespoke code per service.

## What it is

A generic **`ToolRegistry` source** that connects to configured MCP servers
and registers each offered MCP tool as a `Tool` implementation (schema from
the MCP tool description, execution = MCP `tools/call` round-trip).

That sentence is the whole architecture: MCP stays a registry *source*,
never an architecture. Everything downstream of the registry — dialects,
scoping, the agent loop, permission checks at dispatch — must keep working
unchanged, which is why the design work is in the constraints, not in the
plumbing.

## Where it hooks in

- `tools_core::ToolRegistry` is composed by the embedder (see pal's
  `runtime::tool_registry` or code-assistant's `register_default_tools`).
  MCP client mode is one more `register_*` call:
  `register_mcp_tools(&mut registry, &config).await` — connect, list tools,
  wrap, register.
- Each wrapped tool carries `ToolSpec::capabilities` like any native tool.
  This is the integration point for both constraints below: capability tags
  are how tools enter scopes, and how the permission layer classifies them.
- Server connections live behind the wrapper (one client per server,
  reconnect on failure); a dead server degrades to tool errors, never a
  crashed agent.

## Constraint 1: scoping — don't offer everything every turn

Local models degrade sharply with large tool sets, and MCP servers are
chatty (a single Jira server can export dozens of tools). Unconstrained
registration would flood every turn's tool list.

- **Per-server allowlist** in the MCP config: only named tools are
  registered at all. Default for a newly added server is *nothing* —
  the embedder opts tools in explicitly.
  ```json
  { "servers": { "jira": { "command": "...", "tools": ["search_issues", "get_issue"] } } }
  ```
- **Scope grouping via capability tags.** Each server (or tool group in the
  config) maps to a `scope:mcp-<server>` tag; `ToolScope::Custom` already
  lets an embedder select such a group per run, and the per-run scope
  override (`start_agent_for_session(..., tool_scope_override)`, commit
  33b12c5) lets a single turn run with a narrowed set. What is still
  missing upstream is *additive* selection — "the agent scope **plus**
  `scope:mcp-jira`" — today a scope override replaces the whole selection.
  Smallest useful upstream change: allow a set of tags where a request
  offers the union of the given scopes.
- Selection policy (which groups are active when) is embedder logic — e.g.
  pal will decide per lane or per conversation topic. Upstream only
  provides the vocabulary.

## Constraint 2: permission policy before the first outward integration

Reading mail is not sending mail. Before any outward-facing MCP tool is
registered, there must be a permission layer the embedder can trust:

- **Classification**: each allowlisted tool is marked read-only or
  outward-facing in the config (MCP has an `annotations.readOnlyHint`, but
  it is a *hint* from the server — the local config must be authoritative).
  Unclassified tools default to outward-facing.
- **Default deny for outward actions**: read-only tools execute directly;
  outward-facing ones (send mail, create ticket) require a confirmation
  through the embedder's channel before dispatch. The natural hook is a
  `ToolInterceptor` (agent_core hooks) that intercepts outward-tagged
  requests and resolves them against an embedder-provided confirmer — the
  agent loop itself stays policy-free.
- Confirmation UX is embedder-specific (pal: ask in the chat channel;
  code-assistant UI: dialog). Upstream provides the interception seam and
  the capability vocabulary (`mcp:read-only` / `mcp:outward`), nothing
  more.

## Non-goals

- No MCP *server* changes (code-assistant already has an MCP server mode;
  this is the client direction).
- No dynamic tool discovery mid-session in round 1 — the tool set is fixed
  at session start (prompt caches and local models both prefer that).
- No attempt to normalize MCP tool schemas beyond what the dialects already
  need; a tool whose schema doesn't survive the dialect conversion is
  skipped with a warning, not massaged.

## Suggested order

1. Upstream: additive scope selection (union of tags) — small, useful
   beyond MCP.
2. Upstream: `register_mcp_tools` with allowlist + per-server scope tag +
   read-only/outward classification.
3. Upstream: outward-confirmation `ToolInterceptor` with a pluggable
   confirmer.
4. pal: config + confirmer over its channel gateway, first server (email,
   read-only tools only) as the dogfooding target.

## Implementation notes (2026-07-05)

What landed (step 2 of the order above, plus code-assistant UI):

- **Protocol via the official SDK.** Instead of extracting `mcp_server`'s
  types into a shared crate, the client is built on `rmcp`
  (`modelcontextprotocol/rust-sdk`, features `client` +
  `transport-child-process` +
  `transport-streamable-http-client-reqwest`) — same choice as codex and
  vtcode. Both stdio and HTTP (streamable) transports are supported.
- **New generic crate `crates/mcp_client`** (depends only on `tools_core`
  + `rmcp`): `McpServersConfig`/`McpServerConfig` (pure data, file I/O is
  the embedder's), `McpTransport` (the untagged stdio/HTTP transport
  choice), `McpServerConnection`, the `DynTool` proxy `McpTool`
  (registry name `mcp__<server>__<tool>`, sanitized, 64-byte cap with
  deterministic hash suffix), and `register_mcp_tools(&mut registry,
  &config, extra_tags)`. A dead server degrades to error tool outputs.
  `discover_tools` provides ephemeral discovery for configuration UIs.

### Transports

A server is reached either as a child process over **stdio** or over an
**HTTP (streamable)** endpoint. The transport is chosen by the shape of the
server's JSON object — an object with a `url` is HTTP, one with a `command`
is stdio (so existing stdio configs stay valid):

```json
{
  "servers": {
    "jira":   { "command": "npx", "args": ["-y", "some-jira-server"] },
    "remote": {
      "url": "https://example.com/mcp",
      "headers": { "Authorization": "Bearer ${REMOTE_TOKEN}" }
    }
  }
}
```

`${VAR}` substitution applies to stdio `env` values and HTTP `headers`
values (both carry secrets); the raw placeholders are what the settings UI
shows and preserves.
- **Tool filter**: per-server `enabled` flag, `enabled_tools` allowlist
  (None = all, deviating from the opt-in-only default proposed above —
  code-assistant is interactive, the settings UI makes per-tool disabling
  cheap) and `disabled_tools` denylist (denylist wins).
- **Capability tags**: every MCP tool carries `mcp`, `scope:mcp-<server>`,
  and the embedder's tags (code-assistant: `scope:agent`,
  `scope:agent-diff`). `ToolSpec` was widened to owned-capable fields
  (`Cow`) for runtime-discovered tools.
- **code-assistant binding** (`code_assistant_core::tools::mcp`):
  `<config_dir>/mcp-servers.json` with `${ENV_VAR}` substitution in `env`
  values; `tools::default_registry_with_mcp()` builds the registry. The
  wiring layers install `tools::ConfigToolRegistry` as the session
  manager's `ToolRegistryProvider`, which rebuilds registries (cached per
  project behind a fingerprint) at the start of every agent run — so config
  changes apply on the next run, not only on restart. A running agent keeps
  the registry it started with. Projects can additionally ship servers in a
  `.mcp.json` at the project root, gated by a trust prompt — see
  [project-scoped-mcp-servers.md](project-scoped-mcp-servers.md).
- **gpui settings page** ("MCP Servers"): expandable card per server with
  enable switch, add/edit/delete (including a transport selector for
  stdio command/args/env vs HTTP url/headers), live tool discovery and
  per-tool toggles persisted to `disabled_tools`.
- **Which servers a session has on** (`tools::mcp_selection`), in three
  layers:
  - *Global*: a server with `"off_in_new_sessions": true` stays enabled,
    but new sessions start with it off (the settings card's "On in new
    sessions" switch). Servers of a project's `.mcp.json` start on.
  - *Project*: `<config_dir>/project-mcp-servers.json` keeps, per
    canonical project root, only the servers whose state differs from
    their global default. Switching a server in any session of the
    project records it there; a switch back to the default removes the
    entry.
  - *Session*: a new session (`SessionManager::new_session_config`)
    records every available server in `enabled_mcp_servers` /
    `disabled_mcp_servers` — the project's choice, else the global
    default — and keeps its own switches. Later changes to the project or
    global defaults leave it alone; only a server it has no state for
    (added since) is set as in a new session. Sessions derived from
    another one copy that session's lists.
- **Server status and restart** (GPUI input-bar MCP menu): every server row
  shows a dot — green connected, amber connecting, red failed (the error on
  hover), grey not connected yet — and a restart button that works in any
  state. A server skipped while loading the configuration (a `${VAR}` that
  cannot be resolved) shows as failed with the reason. `ConfigToolRegistry` records each server's `McpServerHealth` from
  its connects and registry builds and publishes changes as the app-scoped
  `UiEvent::McpServerStatusChanged`; asking for the statuses
  (`SessionService::refresh_mcp_server_statuses`, sent when the menu opens)
  also reports a server whose process exited as failed
  (`McpServerConnection::is_closed`). A restart
  (`SessionService::restart_mcp_server`, through the
  `session::manager::McpServerControl` seam) closes the server's pooled
  connections — calls in flight on them fail — drops all cached
  registries, and rebuilds the session's registry, which the session then
  holds so the new connection stays alive. Other sessions pick it up on
  their next run.

### OAuth logins

An HTTP server that demands OAuth (it answers `initialize` with `401`) and
has no stored login contributes no tools and shows as *login required*
(`McpServerHealth::NeedsLogin`). The browser login starts from:

- the *Log in* button of that server's row in the input row's *Tools* menu
  (`SessionService::login_mcp_server`), which also restarts the server so
  its tools join the session — this covers servers from the project's
  trusted `.mcp.json`;
- the *Authenticate* action in *Settings → MCP* (global servers);
- `code-assistant mcp-login <server>`, which finds servers in
  `mcp-servers.json` and in the `.mcp.json` of the current directory.

Tokens are stored per server name, wherever the server is configured. A
login never starts on its own for a server that was never logged in.

### Expired OAuth logins

An HTTP server that uses OAuth keeps its tokens in
`<config_dir>/mcp-oauth/<server>.json`; rmcp refreshes the access token
silently. When that fails too (the refresh token expired or was revoked),
the login is renewed automatically, unless `"auto_reauthorize": false` is set
at the top level of `mcp-servers.json` (the settings page's "Log in again
when a login expires" switch):

- **During a tool call**: a call that fails at the transport reopens the
  session (`McpServerConnection` swaps it under its tools). When the server
  demands authorization again, the connection asks its
  `mcp_client::Reauthorizer` — code-assistant's `AutoReauthorizer` runs the
  browser login — reconnects and retries the call. A call that failed for
  another transport reason is not retried, since it may have run.
- **At connect** (e.g. the login expired while the app was closed): the
  server contributes no tools to that run; the login starts in the
  background, and the new token file rebuilds the registry for the next run.

Only servers with a previous login are renewed, global and project-local
alike (the renewal uses the configuration the connection was made with). Concurrent requests for one
server share one browser login, which runs to completion even if the tool
call that started it is cancelled. With the switch off, the tool call fails
with a hint to log in from the settings or with `code-assistant mcp-login`.

Still open from this note: additive scope selection (step 1),
read-only/outward classification and the outward-confirmation
`ToolInterceptor` (step 3). Until step 3 lands, outward-facing MCP tools
run without a confirmation gate — configure conservative allowlists.
