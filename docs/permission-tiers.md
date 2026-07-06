# Permission Tiers

Permission tiers decide **when the agent asks the user before running a
tool**. They are orthogonal to the sandbox: the sandbox constrains what a
command can touch once it runs, the tier decides whether a tool call runs at
all without confirmation.

## Tiers

| Tier (serde id) | Behavior |
|---|---|
| `bypass-all` (default) | Never ask; every tool call runs without prompting. |
| `outward-tools` | Ask before any tool tagged `outward` — tools whose effects leave the machine and are visible to third parties (sending a message, calling a remote service). The tag wins over `read_only`: reading via an outward service still leaks the request. Local writes run without prompting. Embedders tag their outward tools (e.g. per MCP server via the extra-capabilities hook). |
| `write-tools` | Ask before any tool **not** tagged `read_only` — file edits, command execution, and untagged tools (e.g. MCP tools without a read-only hint) count as writes. |
| `all-tools` | Ask before every tool call. |

The tier is stored per session in `SessionConfig.permission_tier` and takes
effect on the next agent run. Answer options on each prompt are **allow
once**, **always allow this tool for this session**, and **deny**. Session
grants live on the `SessionInstance` (shared with sub-agents) and survive
across agent runs but are not persisted. A denial is returned to the LLM as
an error tool result telling it not to retry.

## Architecture

- **Decision + gate**: `tools_core::permissions` — `PermissionTier`
  classifies via the `read_only` / `outward` capability tags; `ToolPermissions`
  (tier + grant set) runs the gate. `agent_core::AgentRuntime` consults it
  in both the sequential and the parallel tool-execution path before
  dispatching a tool.
- **Prompt transport**: the existing `PermissionMediator` seam. Which
  mediator is used depends on the frontend:
  - **GPUI / terminal** (via `SessionService`):
    `SessionPermissionMediator` publishes
    `UiEvent::RequestToolPermission` on the broadcast stream and awaits
    `SessionService::respond_permission`. Open requests are included in
    session snapshots and resolve as *denied* when the user stops the agent
    or a new run starts.
  - **ACP**: `AcpPermissionMediator` uses the protocol's
    `session/request_permission` RPC.
- **Escalations stay separate**: `execute_command`'s explicit
  `ask_user_approval` (sandbox bypass) keeps using the mediator directly,
  independent of the tier.

## Frontend surfaces

- **GPUI**: "Permissions" dropdown in the input-area selector row; incoming
  requests render as a banner above the input with *Allow once / Always
  (session) / Deny*.
- **Terminal**: `/permissions [bypass-all|outward-tools|write-tools|all-tools]`
  shows or switches the tier. An incoming request opens a modal prompt above the
  composer (arrow keys + Enter: *Allow once / Always allow (session) /
  Deny*), one at a time, oldest first. Esc dismisses the prompt without
  answering; the request stays pending (info banner) and can still be
  answered with `/allow`, `/always`, or `/deny`.
- **ACP**: the tiers are advertised as session modes (`bypass-all`,
  `outward-tools`, `write-tools`, `all-tools`) in `new_session`/`load_session`; clients
  switch via `session/set_mode`. Prompts arrive as native ACP permission
  requests.
- **MCP server / headless**: no mediator is available; under an asking tier
  a gated tool call is denied with an explanatory error, so keep
  `bypass-all` there.
