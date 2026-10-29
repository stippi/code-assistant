---
title: Providers and models
weight: 10
lede: A provider holds the credentials and the endpoint. The model selection decides which of its models the picker offers. Everything else about a model is worked out automatically.
frontends: [app, tui, acp]
---

Both files are written by **Settings → Providers** and **Settings → Models**.
You can also edit them by hand.

## `providers.json`

Each entry's key is the **provider ID** that models refer to:

```json
{
  "anthropic-main": {
    "label": "Anthropic Claude",
    "provider": "anthropic",
    "config": {
      "api_key": "${ANTHROPIC_API_KEY}",
      "base_url": "https://api.anthropic.com/v1"
    }
  },
  "openai-chatgpt": {
    "label": "ChatGPT Subscription",
    "provider": "openai-responses-ws",
    "config": { "codex_auth": true }
  },
  "ollama-local": {
    "label": "Ollama",
    "provider": "ollama",
    "config": { "base_url": "http://localhost:11434" }
  }
}
```

| `provider` | Service |
|---|---|
| `anthropic` | Anthropic Messages API (also compatible proxies via `base_url`) |
| `openai` | OpenAI Chat Completions (and compatible APIs) |
| `openai-responses`, `openai-responses-ws` | OpenAI Responses API over HTTP or WebSocket. With `codex_auth`, a ChatGPT subscription |
| `vertex` | Google Gemini (Vertex AI / Generative Language API) |
| `ollama` | Local models through Ollama |
| `openrouter` | OpenRouter |
| `groq`, `cerebras`, `mistral-ai` | Groq, Cerebras, Mistral |
| `ai-core` | SAP AI Core deployments (Anthropic, OpenAI and Vertex models) |

`${NAME}` is replaced with the environment variable `NAME`. The repository's
[`providers.example.json`](https://github.com/stippi/code-assistant/blob/main/providers.example.json)
has a complete example for every provider type.

### ChatGPT subscription

```bash
code-assistant codex-login    # opens the browser
code-assistant codex-status
code-assistant codex-logout
```

## `model-selection.json`

**Settings → Models** lists the models each provider offers. The list comes
from the provider and is refreshed daily, or on demand with **Refresh**.
Switch on the ones the picker should offer. A model's key is
`<provider-id>/<model-id>`.

A built-in catalog and the provider's listing supply the rest: the context
window, the output limit, the preferred edit format and the reasoning effort
levels. An entry only holds what you want to change:

```json
{
  "models": [
    { "provider": "anthropic-main", "id": "claude-opus-5-5" },
    { "provider": "anthropic-main", "id": "claude-sonnet-5-5", "default_effort": "medium" },
    {
      "provider": "my-proxy",
      "id": "corp-claude-opus",
      "name": "Claude Opus (proxy)",
      "behaves_like": "claude-opus-5-5"
    },
    {
      "provider": "ollama-local",
      "id": "qwen3.8:27b-mlx",
      "context_token_limit": 128000,
      "config": { "temperature": 0.7 }
    }
  ]
}
```

| Field | Meaning | In Settings |
|---|---|---|
| `name` | Display name | Name |
| `behaves_like` | A catalog model to take capabilities from, for IDs the catalog doesn't know (a proxy's name for a known model) | Behaves like |
| `context_token_limit` | Context window override (also the basis for [compaction](/docs/long/context/)) | Context window |
| `edit_format` | `simple` (`edit` tool) or `diff` (`replace_in_file`) | Edit format |
| `default_effort` | Effort for sessions that didn't choose one | Default effort |
| `config` | Extra request fields, merged into every request as a JSON Merge Patch (`null` removes a field) | Extra fields |

If there is no `model-selection.json` yet, the entries of an older
`models.json` are imported. The new file is written with your first change.

## Check the setup

```bash
code-assistant --list-providers
code-assistant --list-models
```

The default model for new sessions is chosen in **Settings → General**. When
you start without `--model`, the default is used, otherwise the first model
offered.
