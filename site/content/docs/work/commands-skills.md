---
title: Use slash commands and skills
linkTitle: Slash commands and skills
weight: 80
lede: Type / at the start of a message for built-in commands and for skills, reusable playbooks for specific tasks.
frontends: [app, tui, acp]
---

## Slash commands

Type <kbd>/</kbd> at the start of the input. A menu lists the commands and the
project's skills. <kbd>Enter</kbd> or <kbd>Tab</kbd> takes the selected entry.

| Command | What it does | App | Terminal | ACP |
|---|---|---|---|---|
| `/goal <criteria>` | Set a goal the agent keeps working on. See [Goals](/docs/long/goals/) | ✓ | ✓ | ✓ |
| `/new [prompt]` | Start a fresh context. See [Fresh context](/docs/long/context/) | ✓ | ✓ | (none) |
| `/handoff [instruction]`, `/compact` | Let the agent write a handoff, then continue from it in a fresh context | ✓ | ✓ | (none) |
| `/<skill> <request>` | Use a skill for this request | ✓ | ✓ | ✓ |

The terminal has more commands for things the app does with buttons. See
[Terminal commands](/docs/reference/slash-commands/).

## Skills

A **skill** is a folder with a `SKILL.md`: instructions for one kind of task,
plus optional scripts, references and templates. The system prompt only lists
each skill's name and description. The agent loads the full instructions when
a task calls for it, or when you invoke a skill directly:

{{< frontends >}}
{{< fe "app" >}}
Type `/` and pick the skill from the menu, then describe the request:
`/release-notes for the changes since v0.9`. The skill's instructions are
added to your message.
{{< /fe >}}
{{< fe "tui" >}}
{{< term >}}[dim]›[/] /release-notes for the changes since v0.9{{< /term >}}
`/skill` shows the list of skills to pick from.
{{< /fe >}}
{{< fe "acp" >}}
Skills are listed as the agent's slash commands, so your editor offers them
when you type `/`.
{{< /fe >}}
{{< /frontends >}}

Skills follow the [Agent Skills format](https://agentskills.io), so skills
written for other agents work here too. Code Assistant also ships a
`skill-creator` skill that helps you write new ones.

Where skills are found, how to write them and how to switch them off is in
[Skills and AGENTS.md](/docs/configure/skills/).
