# MCP client mode — design note

Status: design note, not scheduled. Written 2026-07-05 for the pal handoff;
pal (the first downstream consumer of the agent stack) wants MCP servers as
its procurement channel for integrations — email, calendars, Jira-like
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
