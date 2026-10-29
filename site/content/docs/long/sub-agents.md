---
title: Delegate to sub-agents
linkTitle: Sub-agents
weight: 10
lede: The agent can hand part of a task to a sub-agent, a separate agent with a fresh context, and get back only its final answer. This keeps the main conversation short and lets research run in parallel.
frontends: [app, tui, acp]
---

## How it works

The agent starts a sub-agent with the `spawn_agent` tool and a written
instruction. The sub-agent works with its own context and its own tool calls.
When it's done, only its **final answer** goes back to the main agent, not
all the files it read along the way.

Sub-agents come in two modes:

| Mode | Tools | Runs |
|---|---|---|
| `read_only` (default) | Searching and reading only | Several at once, in parallel |
| `default` | All tools, including edits and commands | One after another |

A read-only sub-agent can also run **in the background**. The main agent
doesn't wait for it. It goes on with other work or ends its turn, and the
answer arrives later as a message starting with `[sub-agent #N]`. If the
session is idle at that point, the message starts a new turn.

You can just ask for it:

> Use three sub-agents in parallel to find every place that reads the config
> file, every place that writes it, and all tests for it. Then propose a
> single loader.

## Watch and control them

{{< frontends >}}
{{< fe "app" >}}
In the conversation, a sub-agent shows up as a card with one line per step
of its work. **Open transcript** shows the full run, with all its tool calls.

Running sub-agents, in the foreground and the background, are listed in a
panel above the input with what they are doing (*Thinking…*,
*Running tools…*), how long they have been running, and **Open** and
**Cancel** buttons. Without this panel, a session whose background agents are
still working would look finished.
{{< /fe >}}
{{< fe "tui" >}}
A line above the composer lists the running sub-agents. Use `/agents` to
control them:

{{< term >}}[dim]›[/] /agents                [dim]# list running sub-agents[/]
[dim]›[/] /agents open 2         [dim]# show #2's transcript (read-only)[/]
[dim]›[/] /agents back           [dim]# return to the session[/]
[dim]›[/] /agents cancel 2       [dim]# or: /agents cancel all[/]{{< /term >}}
{{< /fe >}}
{{< fe "acp" >}}
A sub-agent shows up as one tool call in your editor, updated with its
progress, and its final answer as the result.
{{< /fe >}}
{{< /frontends >}}

A cancelled sub-agent returns *“Sub-agent cancelled by user.”* to the main
agent, which then carries on.

{{< callout note "Permissions apply" >}}
Sub-agents use the session's [permission tier](/docs/work/permissions/).
Their permission requests come through the same prompt as the main agent's.
{{< /callout >}}

To keep the agent from using sub-agents, switch off the **Sub-agents** group
in the [Tools menu](/docs/configure/tools/).
