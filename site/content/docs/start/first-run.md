---
title: Connect a model
weight: 20
lede: On first launch the app opens Settings. Add a provider, switch on a model, and you're ready.
frontends: [app, tui, acp]
---

Code Assistant works with many LLM providers: Anthropic, OpenAI (with an API
key or a ChatGPT subscription), Google Vertex AI, Ollama, OpenRouter, Groq,
Cerebras, Mistral, SAP AI Core and others. You need at least one provider and
one model.

{{< frontends >}}
{{< fe "app" >}}
{{< step 1 "Add a provider" >}}
In **Settings → Providers**, use **Quick Setup** for the common providers.
Pick one, paste the API key and click **Set Up**. For other providers, click
**Add Provider**, choose the **Type**, give it a **Label** and fill in the
fields. For a ChatGPT subscription, the app opens a browser login instead of
asking for a key.
{{< /step >}}
{{< step 2 "Switch on models" >}}
**Settings → Models** lists the models each provider offers. Switch on the
ones you want in the model picker. You can filter by name, or add a model ID
the provider doesn't list.
{{< /step >}}
{{< step 3 "Pick a default" >}}
In **Settings → General**, choose the **Default model** for new sessions.
Click **Done** to go to the main window.
{{< /step >}}
{{< /fe >}}
{{< fe "tui" >}}
The terminal reads the same files as the app. The quickest route is to set
up the provider once in the app. Without the app, write
`~/.config/code-assistant/providers.json` by hand:

```json
{
  "anthropic-main": {
    "label": "Anthropic Claude",
    "provider": "anthropic",
    "config": { "api_key": "${ANTHROPIC_API_KEY}" }
  }
}
```

Then check what is available and start with a model:

```bash
code-assistant --list-providers
code-assistant --list-models
code-assistant --tui --model anthropic-main/claude-sonnet-5-5
```

For a ChatGPT subscription, log in with `code-assistant codex-login`.
{{< /fe >}}
{{< fe "acp" >}}
The ACP agent reads the same `providers.json` as the app and the terminal.
Set up a provider in the app, or write the file by hand (see the Terminal
tab). Your editor's agent panel then offers the configured models in its
model picker. The model is chosen per session.

You can also pass API keys through the environment in your editor's agent
settings. See [Editors via ACP](/docs/frontends/acp/).
{{< /fe >}}
{{< /frontends >}}

{{< callout note "Keys in environment variables" >}}
Every value can name an environment variable as `${NAME}`. The key then stays
in your shell profile or a secrets manager, not in the file.
{{< /callout >}}

More options, such as custom endpoints, context-window overrides and extra
request fields, are in [Providers and models](/docs/configure/providers/).
