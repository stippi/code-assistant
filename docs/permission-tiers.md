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
| `auto` | A reviewing model judges each call that may change something against what the user asked for; the user is only asked about calls it holds back. See [Auto mode](#auto-mode). |

The tier is stored per session in `SessionConfig.permission_tier` and takes
effect on the next agent run. A stored tier this build doesn't know (written
by a newer build, or since removed) loads as `all-tools` rather than failing
the session load or silently dropping to `bypass-all`. Answer options on each prompt are **allow
once**, **always allow this tool for this session**, and **deny**. Session
grants live on the `SessionInstance` (shared with sub-agents) and survive
across agent runs but are not persisted. A denial is returned to the LLM as
an error tool result telling it not to retry.

## Auto mode

Under `auto`, tools tagged `read_only` and file edits (`edits_files`) run
without review, unless they are also `outward`. Every other call goes to a
`tools_core::ToolCallReviewer` first:

- **allow** → the call runs, nobody is asked;
- **block** → the user gets the ordinary prompt, with the reviewer's reason
  in its summary (`PermissionRequestReason::AutoModeBlocked`); without a
  mediator the reason goes back to the model as the tool error;
- the review fails (no answer within 60 s, unreadable verdict, provider
  error) or no reviewer is set → treated like a block.

Session grants (*Always (session)*) skip the review for that tool too.

A call the reviewer allowed also has its own escalations answered: the
reviewer saw the parameters, so `execute_command`'s `ask_user_approval`
(run outside the sandbox) is granted without a second prompt
(`tools_core::ReviewedCallMediator`, installed per invocation when
`ToolPermissions::check` returns `Clearance::Reviewed`). Other questions a
tool asks still go to the user.

The reviewer sees the user's messages on the active path (text only, no
tool results — text the agent merely read must not authorize anything; a
compaction summary is included, marked as such), the session's project
directory, and the call (tool name, first line of its description,
parameters). Long histories keep the first message and the latest ones.
It answers `{"decision": "allow" | "block", "reason": "…"}`.

The configuration is central, for all projects and providers, in
`<config_dir>/auto-mode.json` (*Settings → General → Auto mode*):

```json
{
  "model": "some-provider/small-fast-model",
  "effort": "low",
  "instructions": "Pushing to my own feature branches is fine."
}
```

- `model`: key of any offered model, also of another provider than the
  session's; unset reviews with the session's own model.
- `effort`: optional reasoning effort of the reviewing model.
- `instructions`: extra rules appended to the reviewer's instructions.

The reviewer (`code_assistant_core::session::auto_mode::LlmToolCallReviewer`)
is installed by the session manager at the start of each run whose tier is
`auto`, re-reading the config, and creates its LLM client on the first
review. Sub-agents share it; their "user messages" are the task their
parent gave them.

## Architecture

- **Decision + gate**: `tools_core::permissions` — `PermissionTier`
  classifies via the `read_only` / `outward` / `edits_files` capability tags;
  `ToolPermissions` (tier + reviewer + grant set) runs the gate. `agent_core::AgentRuntime` consults it
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
  independent of the tier — except under `auto`, where a call the reviewer
  allowed gets its escalation granted (see above).

## Frontend surfaces

- **GPUI**: "Permissions" dropdown in the input-area selector row; incoming
  requests render as a banner above the input with *Allow once / Always
  (session) / Deny*.
- **Terminal**: `/permissions [bypass-all|outward-tools|write-tools|all-tools|auto]`
  shows or switches the tier. An incoming request opens a modal prompt above the
  composer (arrow keys + Enter: *Allow once / Always allow (session) /
  Deny*), one at a time, oldest first. Esc dismisses the prompt without
  answering; the request stays pending (info banner) and can still be
  answered with `/allow`, `/always`, or `/deny`.
- **ACP**: the tiers are advertised as session modes (`bypass-all`,
  `outward-tools`, `write-tools`, `auto`, `all-tools`) in `new_session`/`load_session`; clients
  switch via `session/set_mode`. Prompts arrive as native ACP permission
  requests.
- **MCP server / headless**: no mediator is available; under an asking tier
  a gated tool call is denied with an explanatory error, so keep
  `bypass-all` there.
