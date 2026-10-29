---
title: Slash commands
weight: 20
lede: All slash commands by frontend.
frontends: [app, tui, acp]
---

| Command | Does | App | Terminal | ACP |
|---|---|---|---|---|
| `/<skill> <request>` | Run a request with a skill's instructions | ✓ | ✓ | ✓ |
| `/goal <condition>` | Set or replace the session's goal | ✓ | ✓ | ✓ |
| `/goal status\|pause\|resume\|cancel` | Manage the goal | ✓ | ✓ | ✓ |
| `/new [prompt]` | Fresh context, starting with `prompt` | ✓ | ✓ | (none) |
| `/handoff [instruction]` | Agent writes a handoff, then continues fresh | ✓ | ✓ | (none) |
| `/compact` | Same as `/handoff` | ✓ | ✓ | (none) |
| `/help`, `/h` | Show the commands | (none) | ✓ | (none) |
| `/model [name]`, `/m` | List or switch models | selector | ✓ | config option |
| `/effort [level]`, `/e` | Show or set the reasoning effort | selector | ✓ | config option |
| `/provider`, `/p` | List providers | (none) | ✓ | (none) |
| `/current`, `/c` | Show the active model | (none) | ✓ | (none) |
| `/permissions [tier]` | Show or set the permission tier | selector | ✓ | session mode |
| `/allow`, `/always`, `/deny` | Answer the pending permission request | banner | ✓ | prompt |
| `/plan` | Toggle the plan panel | (none) | ✓ | (none) |
| `/clear` | Clear the conversation context | (none) | ✓ | (none) |
| `/agents [open <id>\|back\|cancel <id\|all>]` | Sub-agents | panel | ✓ | (none) |
| `/sessions`, `/resume` | Switch session | sidebar | ✓ | (none) |
| `/skill` | Pick a skill from the list | `/` menu | ✓ | (none) |
