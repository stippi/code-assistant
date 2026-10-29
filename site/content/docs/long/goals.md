---
title: Set a goal and let it keep going
linkTitle: Goals
weight: 20
lede: Give the session a completion condition with /goal. After each turn, Code Assistant checks whether the condition holds and, if it doesn't, starts another turn.
frontends: [app, tui]
---

A normal turn ends when the agent thinks it's done. A **goal** sets the bar:
after each turn, the result is checked against your condition, using what
actually happened in the session (commands run and their output, files
produced). If the goal isn't met, the agent gets another turn. This repeats up
to **10 turns**.

## Set a goal

Type `/goal` and the condition, phrased so it can be checked:

{{< term >}}[dim]›[/] /goal cargo test passes and cargo clippy reports no warnings
[acc]Goal set: cargo test passes and cargo clippy reports no warnings
The controller pursues it (up to 10 turns). Use /goal cancel to remove it.[/]{{< /term >}}

Each session has at most one goal. A new `/goal` replaces the current one.

## Manage it

| Command | Effect |
|---|---|
| `/goal <condition>` | Set or replace the goal |
| `/goal status` | Show the goal and its attempts |
| `/goal pause` | Stop starting new turns for now |
| `/goal resume` | Continue |
| `/goal cancel` | Remove the goal |

Goals are stored with the session, so they survive a restart. Turns are only
started while the app or the terminal is running. Nothing continues in the
background after you quit.

{{< callout tip "Good conditions" >}}
Name something observable: “the e2e suite passes”, “`npm run build` succeeds
and the bundle is under 200 KB”. A vague goal like “the code is clean” is hard
to check and will use up the turns.
{{< /callout >}}

In ACP editors `/goal` is accepted and stored, but the extra turns are only
driven by the app and the terminal.
