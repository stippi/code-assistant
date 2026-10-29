---
title: Follow what the agent does
linkTitle: Follow the conversation
weight: 20
lede: Each step the agent takes is visible, so you can see what it did, why, and what it saw.
frontends: [app, tui, acp]
---

## Tool calls

The agent works through **tools**: it searches, reads files, edits, runs
commands, browses. Every call is shown with its input and its result, the same
text the model gets back.

{{< frontends >}}
{{< fe "app" >}}
Each call is a **card**. A card stays compact while the call runs and can be
expanded afterwards:

- **Edits** show a diff with syntax highlighting.
- **Commands** show their output live, in color, like a terminal.
- **Search and read** show the matches and file contents the model got.
- **Sub-agents** show one line per step, with **Open transcript** for the
  full run.
- **MCP tools** show the server, the tool and its result.

Quick calls like reads and searches take one line each. Click the line to
expand it.
{{< /fe >}}
{{< fe "tui" >}}
Each call is shown inline in the transcript with its name, the main
parameter and the status. Edits render as diffs, commands show their
output.
{{< /fe >}}
{{< fe "acp" >}}
Tool calls are shown as your editor's tool call entries. Edits appear as
diffs, commands run in the editor's terminals, and file reads go through the
editor, so the agent also sees unsaved changes.
{{< /fe >}}
{{< /frontends >}}

## Reasoning

Models that reason show their thinking in a collapsible block above the answer.
How much a model reasons is set by the session's
[reasoning effort](/docs/work/models/).

## The plan

For longer tasks the agent keeps a **plan**, a short list of steps marked
pending, in progress or done. It updates the plan as it goes.

{{< frontends >}}
{{< fe "app" >}}The plan is shown as a block in the conversation and updates in place.{{< /fe >}}
{{< fe "tui" >}}Toggle the plan panel with `/plan`.{{< /fe >}}
{{< fe "acp" >}}The plan is sent to your editor, which shows it in its own plan view.{{< /fe >}}
{{< /frontends >}}

## Steering while it works

- **Send another message any time.** It is queued (shown as *Pending*) and
  goes to the model at the agent's next step, without stopping the run.
- **Stop** with <kbd>Esc</kbd> or the **Stop** button. The agent stops at the
  next safe point.
- **Resume.** When a run ended early (stopped, an error, the app was closed),
  a **Resume** button continues it.

## Questions from the agent {#questions}

When the agent needs a decision from you, it can ask up to four
multiple-choice questions at once. In the app they appear as a form above the
input. Each question has radio buttons or checkboxes and an optional comment
field. Click **Submit answers**, or **Skip** to let it continue without
answers. The session shows *Needs answer* in the inbox while it waits.

This tool is offered only in the app, so the agent never waits for an answer
in a frontend that can't show the form.

## Editing a message: branches {#branches}

{{< fe "app" >}}
Hover one of your earlier messages and click the edit button to change it and
send it again. The conversation continues from there as a new **branch**. The
old branch is kept. Arrows at the bottom of the message switch between
branches.
{{< /fe >}}
{{< fe "tui" >}}Branches created in the app are shown on their active path. Editing earlier messages is done in the app.{{< /fe >}}
{{< fe "acp" >}}Branches created in the app are shown on their active path. Editing earlier messages is done in the app.{{< /fe >}}

## Rate limits and errors

When a provider answers with a rate limit, the agent waits and retries on its
own. The sidebar shows *Rate limited* or *Retrying*. Other errors end the run
with a message, and you can **Resume** once the cause is fixed.
