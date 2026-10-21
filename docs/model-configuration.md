# Models, capabilities and reasoning effort

How the app knows what a model can do, which models the picker offers, and
how a session's reasoning effort reaches the provider. User-facing setup is in
[configuration.md](configuration.md#providers-and-models).

## Pieces

| Piece | Where | Role |
|---|---|---|
| `Effort` | `llm::capabilities` | Provider-independent level: `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` |
| `ReasoningSpec` / `ReasoningStyle` | `llm::capabilities` | The levels a model supports, its default, and how a level is written into a request |
| Catalog | `llm/src/capabilities/catalog.json` | Hand-kept list of the models we use: name, context window, output limit, edit format, reasoning; aliases for proxy model IDs |
| Fallback rules | `llm::capabilities::fallback` | Reasoning for models nobody knows, by API family (Responses API models reason, Chat Completions only for known reasoning families, Anthropic never guessed) |
| Discovery | `llm::discovery` | Lists a provider's models (`/models` per API, Ollama tags, AI Core deployments); cached in `model-discovery.json` (`DiscoveryCache`) |
| Model selection | `llm::model_selection` | `model-selection.json`: the models the picker offers, keyed `provider/id`, with the user's overrides |
| `ConfigurationSystem` | `llm::provider_config` | Providers + selection + discovery; resolves a model by key or name, its display name and its capabilities |

## Resolving capabilities

`ConfigurationSystem::model_capabilities(model_ref)`:

1. `ModelCapabilities::resolve(api, id, discovered)` — the catalog entry for
   the model's ID (or its `behaves_like`), with gaps filled from the provider
   listing; without a catalog entry, the listing alone plus the fallback
   reasoning rules.
2. The selection entry's overrides: `context_token_limit`, `edit_format`,
   `default_effort` (moved to the closest supported level), `name`.

Context limits for compaction and the context ring, the edit format a new
session locks in, and the effort levels a selector offers all come from here.

## From effort to request

`SessionModelConfig { model_name, effort }` is stored per session; `None`
runs the model's default. The effort survives model switches: requests run at
the closest level the current model supports (`ReasoningSpec::effective_effort`).

The factory (`create_llm_client_from_model`) builds the request overlay:

```
capabilities.request_overlay(api, effort)   // thinking / output_config / reasoning / ...
  ⊕ model.config                           // user's extra fields
```

Both are JSON Merge Patches (RFC 7386, `config_merge::merge_patch`); clients
merge the overlay into their request as the last step and otherwise send
neutral requests (no reasoning defaults of their own). Two things are derived
from the final request: the Anthropic client adds the interleaved-thinking
beta header for budget-based thinking, the Responses clients ask for
encrypted reasoning when the request has a `reasoning` object.

Frontends get `UiEvent::UpdateEffort { choice: EffortChoice }` (levels of the
session's model + the effective level) with the session snapshot, on
`SessionService::switch_model` and on `SessionService::set_effort`.

## Model references

Sessions, `default-model.json` and `--model` refer to a model by key
(`provider/id`). Older sessions name it by display name; `resolve_key`
resolves both, the run preparation heals the stored reference to the key, and
snapshots report the key. Display names come from the user's `name`, else the
catalog or listing, else the ID; two offered models with the same name get the
provider label appended.

`model-selection.json` replaces `models.json`. When it is missing, the old
file's entries are imported (`ModelSelection::import_legacy`): names stay,
reasoning fields move into `default_effort`, limits the catalog already knows
are dropped. The old file is not touched, so older builds keep working.

## Adding a model to the catalog

Add an entry to `catalog.json` — `id`, `name`, `context_window`,
`max_output`, `edit_format` if it needs the diff guard, and `reasoning` with
the style, the supported `levels` (ascending, `off` only if reasoning can be
switched off) and the `default`. Proxy IDs for the same model go into
`aliases`. The catalog tests check that every default is a supported level.
