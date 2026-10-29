---
title: Choose model and reasoning effort
linkTitle: Model and effort
weight: 30
lede: Each session has its own model and reasoning effort. You can change both at any point, and the next request uses the new choice.
frontends: [app, tui, acp]
---

## Pick a model

{{< frontends >}}
{{< fe "app" >}}
Click the model in the selector row under the input. Models are grouped by
provider. The list shows the models you switched on in **Settings → Models**.

{{< selectors on="Claude Sonnet" />}}
{{< /fe >}}
{{< fe "tui" >}}
{{< term >}}[dim]›[/] /model                                  [dim]# lists the models you can pick[/]
[dim]›[/] /model anthropic-main/claude-opus-5-5
[acc]Switched to model: Claude Opus 5.5[/]{{< /term >}}

`/current` shows the active model, `/provider` lists the providers. Start
with a model directly with `--model <name or key>`.
{{< /fe >}}
{{< fe "acp" >}}
The model is a session **config option**. Your editor shows it as a model
picker grouped by provider (in Zed, next to the agent's input). You can also
set the starting model in the agent's command:
`"args": ["acp", "--model", "anthropic-main/claude-sonnet-5-5"]`.
{{< /fe >}}
{{< /frontends >}}

A model is identified by its key, `<provider-id>/<model-id>`, or by its
display name.

## Reasoning effort

Models that can reason let you choose how much they think before they answer.
Higher effort gives better results on hard problems but is slower and uses
more tokens. The levels offered depend on the model:

`off` · `minimal` · `low` · `medium` · `high` · `xhigh` · `max`

{{< frontends >}}
{{< fe "app" >}}Use the **Effort** dropdown next to the model. It is shown only for models that reason.{{< /fe >}}
{{< fe "tui" >}}`/effort` shows the current level and the ones available. `/effort high` sets it.{{< /fe >}}
{{< fe "acp" >}}A second config option, *thought level*, appears next to the model picker for models that reason.{{< /fe >}}
{{< /frontends >}}

Code Assistant turns the level into what each provider expects, for example
`thinking` for Anthropic, `reasoning.effort` for the OpenAI Responses API and
`thinking_level` for Gemini. You never have to write those fields yourself.

{{< callout tip "Defaults per model" >}}
A session that never chose an effort uses the model's `default_effort` from
`model-selection.json`, or the model's own default. See [Providers and models](/docs/configure/providers/).
{{< /callout >}}

## Tool syntax

Most models use the provider's native tool calling. For models that handle
tools badly, a session can describe tools in the system prompt instead and
parse them from the answer:

| Syntax | How tools are called |
|---|---|
| `native` (default) | The provider's function calling |
| `xml` | XML tags in the answer. Parameters stream as they are written |
| `caret` | Blocks fenced with `^^^`. Uses fewer tokens and also streams |

Choose it with `--tool-syntax` at startup. A session keeps the syntax it was
created with.
