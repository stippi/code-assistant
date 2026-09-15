# Context Compaction

When the conversation nears the model's context window, the agent asks the
model for a hand-off and continues in a fresh context that starts from it.
The full history stays in the session and the UI; only the prompt sent to
the LLM is trimmed.

## Trigger

- `CompactionPolicy` (`crates/agent_core/src/hooks.rs`) decides when and
  with which prompt. The domain implementation is `TokenRatioCompaction`
  (`crates/code_assistant_core/src/plugins/compaction.rs`): it compacts once
  the last assistant turn's usage (input + cache write + cache read +
  output) reaches 80% of the model's `context_token_limit` from
  `models.json`. An unknown limit disables compaction for the run.
- The check runs at the top of every loop iteration in
  `AgentRuntime::run_until_complete` (`crates/agent_core/src/runtime.rs`),
  **before** a pending user message is appended. A request that arrives
  while the context is full therefore follows the hand-off instead of being
  folded into it.

## The compaction request

`request_handoff` sends the current prompt plus one user message holding
the compaction prompt
(`crates/code_assistant_core/resources/compaction_prompt.md`). The prompt
frames the task as a hand-off to another instance: what the user asked for
and expects, progress and decisions, verified facts with file paths, next
steps, open questions. It forbids tool calls.

The request keeps the system prompt and the tool definitions unchanged.
Dropping the tools would change the cached prompt prefix (tools → system →
messages) and make the whole request a cache miss; a `tool_choice` change
would still invalidate the messages block. If the model answers with tool
calls instead of text anyway, the request is repeated once with a reminder
appended; a second failure fails the turn rather than storing an empty
summary. Only text blocks count as the hand-off; thinking blocks are
ignored.

## Storage

The hand-off text is appended as a user message flagged
`is_compaction_summary`. The policy may append an addendum
(`post_compaction_summary_addendum`), e.g. a reminder of the skills that
were loaded before compaction dropped their tool results. The UI receives
a `DisplayFragment::CompactionDivider` with the summary text and renders a
collapsible banner; the divider does not include the addendum.

## Prompt after compaction

`prompt_messages` builds the request from the last summary node onwards
and rewrites the summary node into the hand-off message
(`crates/agent_core/src/runtime/handoff.rs`):

```
<handoff>
<preamble: a previous instance hit the context limit and wrote this;
 build on its work>

<user_messages>
<message index="1">…verbatim…</message>
…
</user_messages>

<summary>
…hand-off text (plus addendum)…
</summary>
</handoff>
```

The user messages are collected from the whole active path before the
summary, so a second compaction still carries the messages from before the
first. Tool-result messages and earlier summaries are skipped (they share
the user role); images are dropped. The newest messages are kept within a
character budget, older ones are counted as omitted. Everything is one
text message, which avoids consecutive user turns that some providers
reject.

## Tests

- `crates/agent_core/src/runtime/handoff.rs` — rendering and message
  selection.
- `crates/agent_core/src/runtime/tests.rs` — prompt projection after one
  and two compactions.
- `crates/code_assistant_core/src/agent/tests.rs` — end-to-end: summary
  insertion, pending message ordering, retry on tool-call answers,
  failure without text, skill reminder addendum.
