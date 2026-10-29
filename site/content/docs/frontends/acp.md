---
title: Editors via ACP (Zed and others)
linkTitle: Editors via ACP
weight: 20
lede: Run Code Assistant as an Agent Client Protocol agent inside Zed or any other ACP client. It works through your editor, with its buffers, terminals and permission prompts.
frontends: [acp]
---

## Set up Zed

Add the agent to Zed's `settings.json`:

```json
{
  "agent_servers": {
    "Code-Assistant": {
      "command": "/path/to/code-assistant",
      "args": ["acp", "--model", "anthropic-main/claude-sonnet-5-5"],
      "env": { "ANTHROPIC_API_KEY": "sk-ant-..." }
    }
  }
}
```

Then pick **Code-Assistant** when you start a new thread in Zed's agent panel.
See [Zed's docs on custom agents](https://zed.dev/docs/ai/external-agents#add-custom-agents).

Other ACP clients work the same way: they start `code-assistant acp` and talk
to it over stdin/stdout.

### Options

| Argument | Default | Meaning |
|---|---|---|
| `--model <name or key>` | the default model | Model for new sessions |
| `--path <dir>` | `.` | Project folder, if the client doesn't send one |
| `--tool-syntax native\|xml\|caret` | `native` | How tools are called ([details](/docs/work/models/#tool-syntax)) |
| `--sandbox-mode …`, `--sandbox-network` | `danger-full-access` | [Command sandbox](/docs/work/permissions/#command-sandbox) |
| `--use-diff-format` | off | Use `replace_in_file` diff blocks instead of `edit` |
| `--verbose` / `-v` | off | Log more |

## What you get in the editor

| Feature | How it shows up |
|---|---|
| **Model and effort** | Session config options: a model picker grouped by provider, plus a *thought level* for models that reason |
| **Permission tiers** | Session modes: `bypass-all`, `outward-tools`, `write-tools`, `all-tools` |
| **Permission requests** | The editor's own prompts |
| **Files** | Read and written through the editor (`fs/read_text_file`, `fs/write_text_file`), so unsaved edits are visible to the agent |
| **Commands** | Run in the editor's terminals when the client supports them, otherwise locally |
| **Tool calls and diffs** | Shown as the editor's tool call entries |
| **Plan** | Sent as plan updates |
| **Skills** | Offered as slash commands, `/<skill-name> <request>` |
| **Goals** | `/goal …` is accepted (see [Goals](/docs/long/goals/)) |
| **Sessions** | Listed and loadable from the editor's history, with titles |
| **Context usage** | Reported to the editor |

Sessions are stored where the app stores them, so a thread started in Zed can
be opened in the app later, for example to use the Review panel.

## Not available through ACP

The interactive `ask_question` form, the live browser panel, the inbox,
worktrees, voice, `/new` and `/handoff` are app or terminal features.
