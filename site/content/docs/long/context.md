---
title: "Start fresh: /new, /handoff and compaction"
linkTitle: Fresh context
weight: 40
lede: Long conversations get slow, expensive and unfocused. Code Assistant can carry the important parts into a fresh context, automatically or when you ask.
frontends: [app, tui]
---

## Automatic compaction

When the conversation reaches **80 %** of the model's context window, the agent
writes a **handoff**: a summary of the task, the decisions made, the current
state and the next steps. It then continues in a fresh context that starts
from that summary. The full history stays visible in the session. Only what
is sent to the model gets shorter.

## `/new [prompt]`

Ends the current context on purpose and starts a fresh one whose first message
is `prompt`. You are asked where it goes:

- **behind a divider in this session** (*New context*), or
- **in a new session** of the same project.

## `/handoff [instruction]`

Alias: `/compact`. The agent first writes a handoff prompt from the current
context. You can steer it with an instruction such as
`/handoff focus on the open test failures`. Then it continues exactly like
`/new <that prompt>`.

## Prepared handoff when idle

When a long session goes quiet for a few minutes after the agent answered, Code
Assistant prepares a handoff while the provider's prompt cache is still
warm. Writing it then is cheaper. The prompt shows up in the composer as
`/new <prompt>`. Send it to continue fresh, or delete it and keep going in the
current context.

Set the size from which this happens in **Settings → General → Handoff when
idle** (in input tokens).

{{< callout note "Editors" >}}
ACP editors manage threads themselves. `/new` and `/handoff` aren't available
there, but automatic compaction works the same.
{{< /callout >}}
