# Plugins Concept

This document proposes a plugin system for code-assistant, modelled on
Claude Code's plugins: a plugin is a git repository, installed by its URL,
that bundles extensions the app already knows how to use (skills, MCP
servers) and a few new ones (file renderers, provider templates, managed
providers, hooks, commands).

Status: a first part is built — installing from git or a folder,
provider templates, read-only MCP servers and web file viewers (draw.io);
see [plugins.md](plugins.md). The manifest that shipped is a single
`plugin.json` with the contributions inline (`providerTemplates`,
`mcpServers`, `fileViewers`); marketplaces, project-declared plugins,
command and WASM renderers, managed providers, hooks and the rest below are
still concepts. Naming: the existing module `code_assistant_core::plugins`
holds the compiled-in agent hooks; the plugin code lives in
`code_assistant_core::plugin_host`. This document uses "plugin" for the
user-facing thing only.

## Summary

- **Install by git URL.** `code-assistant plugin add https://github.com/acme/diagrams`
  (or *Settings → Plugins → Add*) clones the repository, reads its manifest,
  shows what it contributes and what it can do, and pins the commit. Updates
  are explicit and show what changed.
- **Marketplaces** are git repositories too: one repository holding
  several plugins in folders, listed in a `marketplace.json`. Adding a
  marketplace makes its plugins installable by name. A marketplace does not
  point to other repositories.
- **One manifest, many contributions.** `plugin.json` declares skills, MCP
  servers, file renderers, provider templates, managed providers, hooks,
  slash commands and more. Each contribution type feeds an existing seam;
  the plugin host only discovers, validates and switches them on and off.
- **Trust is explicit and pinned.** Installing shows the capabilities
  (launches processes, opens network hosts, adds a provider, ...). A
  project can declare the plugins it wants; that goes through the same kind
  of trust prompt as a project's `.mcp.json`. Managed providers never carry
  secrets in the repository.

## Goals and non-goals

Goals:

- Extend the app without recompiling it, by people who do not write Rust.
- Reuse the formats the app already reads: Agent Skills and `.mcp.json`.
- Make every contribution switchable globally, per project and per session,
  the way MCP servers and built-in tool groups already are
  (`docs/builtin-tool-toggles.md`).
- A team can hand out a provider setup (endpoint, models, login) without
  handing out keys.

Non-goals (first release):

- Native code in plugins (Rust dylibs). No stable ABI, and it would run
  with full rights inside the app. Process-based (MCP, commands) and
  web-based (renderers) contributions cover the use cases.
- Plugins changing the GPUI layout freely. They contribute into fixed
  slots (renderer dropdown, settings forms, tools menu).
- A hosted plugin registry. Git is the distribution channel.
- Compatibility with Claude Code plugins and marketplaces
  (`.claude-plugin/`). The concept borrows their ideas, not their format;
  reading their manifests can be added later if it proves useful.

## Installing

### Sources

| Source | Example | Notes |
|---|---|---|
| Git URL | `https://github.com/acme/diagrams` | default branch, pinned to its head commit |
| Git URL + ref | `https://github.com/acme/diagrams#v1.2.0` | tag, branch or commit |
| Shorthand | `acme/diagrams` | GitHub |
| Subfolder | `https://github.com/acme/tools#main:plugins/drawio` | monorepos |
| Marketplace entry | `diagrams@acme` | a folder of a marketplace repository, after `plugin marketplace add acme/plugins` |
| Local folder | `~/dev/my-plugin` | for development; linked, not copied, reloaded on change |

Cloning uses the user's git setup (shell out to `git`, so SSH keys,
credential helpers and corporate proxies work for private repositories).

### Layout on disk

```
<config_dir>/plugins/
  plugins.json                   # installed plugins, pins, enabled state
  marketplaces.json              # added marketplaces
  repos/<host>/<owner>/<repo>/   # one clone per repository (fetch target)
  checkouts/<plugin>/<commit>/   # worktree of the pinned commit, read-only
  data/<plugin>/                 # writable state a plugin may keep
```

`plugins.json`:

```jsonc
{
  "plugins": {
    "diagrams": {
      "source": "https://github.com/acme/diagrams",
      "ref": "v1.2.0",
      "commit": "3f9c1e2…",
      "enabled": true,
      "granted": ["process", "network:embed.diagrams.net"]
    },
    "mermaid": {
      "marketplace": "acme",               // installed from a marketplace folder
      "path": "plugins/mermaid",
      "commit": "a71d0b4…",                // the marketplace's commit
      "enabled": true,
      "granted": ["process"]
    }
  }
}
```

The checkout of a pinned commit never changes under a running session.
Updating fetches, shows the manifest diff (new contributions, widened
capabilities) and, after confirmation, switches the pin; the old checkout is
removed when no session still uses it.

### Project-declared plugins

A project lists the plugins it expects in `.agents/plugins.json` (next to
`.agents/skills/`):

```json
{ "plugins": { "diagrams": { "source": "acme/diagrams", "ref": "v1" } } }
```

Opening such a project offers to install or enable them. Like `.mcp.json`
(`docs/project-scoped-mcp-servers.md`), the file arrives with a clone and
may launch processes, so it is never acted on silently: the prompt names
the plugins and their capabilities and offers *Once*, *Always (this
project)* and *Deny*; trust is stored by project path and file fingerprint
(the `mcp-trust.json` mechanism, generalized).

### Marketplaces

A marketplace is a repository that holds several plugins, each in its own
folder, listed in `.code-assistant-plugin/marketplace.json` at its root:

```json
{
  "name": "acme",
  "owner": { "name": "ACME Platform Team" },
  "plugins": [
    { "name": "diagrams", "path": "plugins/diagrams", "description": "draw.io renderer and a diagram skill" },
    { "name": "mermaid", "path": "plugins/mermaid", "description": "Mermaid renderer" },
    { "name": "acme-gateway", "path": "plugins/acme-gateway", "description": "ACME LLM gateway" }
  ]
}
```

- Entries are folders inside the marketplace repository only; there are no
  links to other repositories. A plugin that lives in its own repository is
  installed by its URL instead. This keeps one clone, one pin and one
  review per marketplace, and a marketplace cannot pull in code from places
  the user never saw.
- Each folder is a complete plugin with its own `plugin.json`; the
  marketplace's `name` for an entry must match the plugin's `name`.
- Plugins from a marketplace share its clone and are pinned to the
  marketplace's commit. Updating the marketplace fetches once and offers
  the updates of the installed plugins whose folders changed (each with its
  manifest diff, as above).
- Removing a marketplace asks whether to keep its installed plugins (they
  stay pinned and stop receiving updates) or remove them.

`marketplaces.json` stores each marketplace's source, ref and commit.
*Settings → Plugins* gets a *Browse* tab listing the plugins of all added
marketplaces with an *Install* button.

## The manifest

`.code-assistant-plugin/plugin.json` at the plugin root. Everything is
optional except `name`; folders with conventional names are picked up
without being listed.

```jsonc
{
  "name": "diagrams",                      // [a-z0-9-], unique per install
  "version": "1.2.0",
  "description": "draw.io and Mermaid renderers, a diagram skill",
  "author": { "name": "ACME", "url": "https://acme.example" },
  "license": "Apache-2.0",
  "engines": { "code-assistant": ">=0.9" }, // refuse to load on older apps

  "skills": "skills/",                     // default: skills/
  "mcpServers": ".mcp.json",               // default: .mcp.json
  "commands": "commands/",                 // default: commands/
  "renderers": [ /* see File renderers */ ],
  "providerTemplates": "providers/",       // default: providers/
  "managedProviders": [ /* see Managed providers */ ],
  "hooks": "hooks/hooks.json",
  "agents": "agents/",
  "formatters": "formatters.json",
  "themes": "themes/",

  "capabilities": {                        // what the plugin may do, shown on install
    "process": true,                       // launches commands / stdio MCP servers
    "network": ["embed.diagrams.net"],     // hosts its renderers may reach
    "providers": true                      // adds provider templates or managed providers
  }
}
```

Paths inside the manifest and in contributed files may use
`${PLUGIN_ROOT}` (the checkout) and `${PLUGIN_DATA}` (the writable data
folder).

Validation is strict like the skills frontmatter: an unknown top-level key
or an invalid contribution disables that contribution with a visible
warning in *Settings → Plugins*, not the whole plugin.

## Contributions

Each type below says what it is and which existing seam takes it.

### 1. Skills

`skills/<name>/SKILL.md`, exactly the Agent Skills format
(`docs/skills-feature.md`). The skills loader gets a fourth scope,
`Plugin`, between `User` and `System`; a project or user skill of the same
name still wins. The catalog shows the plugin as the skill's origin, and
`skills.json`'s `disabled` list works on plugin skills too.

### 2. MCP servers

`.mcp.json` in the plugin root, the format the project file already uses
(`mcp_client::parse_local_mcp_json`), plus `${PLUGIN_ROOT}`. The servers
join the effective set between global and project servers. To keep tool
names unique, a plugin server is registered as `<plugin>-<server>` (just
`<plugin>` when the plugin has a single server of the same name), so tools
read `mcp__diagrams-drawio__export`.

`ConfigToolRegistry`'s fingerprint includes the plugin lock (pinned commits
and enabled state), so installing, updating or switching a plugin rebuilds
the registry at the next run; health, restart and OAuth login work as for
any server. The *Tools* menu groups plugin servers under their plugin.

Typical use: a plugin wraps a CLI as a stdio MCP server and ships the skill
explaining when to use it.

### 3. File renderers

The Files view already switches between **Text** and renderers
(`docs/file-renderers.md`); its `RENDERERS` table is static. Plugins add
entries to it. `FileRenderer`'s `fn` pointers become boxed closures (or a
`RendererFactory` trait object) so entries can be built at runtime, and the
table becomes `Vec<FileRenderer>` built from built-ins plus enabled
plugins. Ids are namespaced (`diagrams/drawio`) so a remembered pick
survives reinstalls.

A renderer declares the files it takes by glob, so `file_kind` should learn
double extensions (`.drawio.svg` vs `.svg`), as the file-renderers doc
already notes. Three kinds:

#### 3a. Command renderers (simple, first)

The file's text goes to a command on stdin; its stdout is shown.

```jsonc
{
  "id": "mermaid",
  "label": "Diagram",
  "files": ["*.mmd", "*.mermaid"],
  "kind": "command",
  "command": ["${PLUGIN_ROOT}/bin/mmdc", "--input", "-", "--output", "-", "--outputFormat", "svg"],
  "output": "svg",                        // svg | png | markdown | html
  "debounceMs": 400
}
```

- `svg` and `png` show through the existing SVG/image path, `markdown`
  through the Markdown renderer's `TextView`, `html` through a web renderer
  (3b) with the output as a static page.
- Runs in the background, re-runs when the editor's text changes
  (debounced), shows stderr in the view when the command fails, and is
  killed after a timeout.
- Good for Mermaid, PlantUML, Graphviz, D2, LaTeX snippets, OpenAPI → HTML,
  Jupyter → Markdown.

Read-only: no line reveal, no comments, like the SVG renderer.

#### 3b. Web renderers (draw.io and other editors)

A web page shows the file and may edit it. The app does not embed a web
view, and the browser panel plan explains why it should not take a native
one (`wry` subviews sit above GPUI's drawing; WebKit speaks no CDP). Web
renderers therefore reuse the browser panel's machinery: the page runs in a
tab of a headless Chrome, drawn into the Files view through the CDP
screencast, with mouse and keyboard forwarded (`web::screencast`,
`web::user_input`). Unlike the agent's browser there is no *Take control*:
input always goes to the page, and the agent never drives these tabs. If
the CEF stage of the browser panel plan happens, renderers get native
popups and lower latency for free.

```jsonc
{
  "id": "drawio",
  "label": "draw.io",
  "files": ["*.drawio", "*.drawio.svg", "*.drawio.png"],
  "kind": "web",
  "page": {
    // one of:
    "bundle": "web/drawio/index.html",                // served from the checkout
    // "url": "https://embed.diagrams.net/?embed=1&proto=json&spin=1",
    // "server": { "command": ["node", "${PLUGIN_ROOT}/server.js"], "readyPattern": "listening on (\\d+)" }
  },
  "adapter": "web/adapter.js",          // bridges the page to the host API below
  "edits": true,                        // the page may write the file back
  "network": ["none"]                   // or a host list; enforced, see Security
}
```

Page sources:

- **`bundle`**: a static folder in the plugin, served by an in-app loopback
  HTTP server on a random port, one origin per plugin
  (`http://127.0.0.1:<port>/<plugin>/<token>/…`), so plugins cannot read
  each other's storage. Works offline. draw.io's web app (Apache-2.0,
  `jgraph/drawio`) can be bundled this way.
- **`url`**: a remote page, e.g. `embed.diagrams.net`. No install size, but
  the file's content goes to that origin's page (it stays in the browser
  for draw.io, but the plugin must declare the host and the install dialog
  says so).
- **`server`**: the plugin starts a local process (like a stdio MCP server,
  same lifetime rules: one per plugin, stopped when the last view closes)
  and the page is loaded from the port it reports. For renderers that need
  a backend (a Kroki instance, a notebook kernel).

The host API, injected into the page before its scripts run
(`Page.addScriptToEvaluateOnNewDocument` plus `Runtime.addBinding`):

```ts
interface CodeAssistantHost {
  // host → page (the page registers handlers)
  onText(cb: (text: string, path: string) => void): void;  // initial load and every reload
  onTheme(cb: (theme: { dark: boolean; colors: Record<string, string> }) => void): void;
  onRevealLine?(cb: (line: number) => boolean): void;
  // page → host
  ready(): void;
  setText(text: string): void;          // only with "edits": true; goes through FileViewer's save path
  setDirty(dirty: boolean): void;
  comment?(range: { startLine: number; endLine: number }, excerpt: string): void;
  openFile(path: string, line?: number): void;               // OpenFileBus
}
```

The adapter maps this onto the page's own protocol. For draw.io's embed
mode that is a few lines: on `init` post `{action: "load", xml, autosave: 1}`;
on `autosave`/`save` call `setText(msg.xml)` (for `.drawio.svg`, request
`{action: "export", format: "xmlsvg"}` and write that). Saving still goes
through `SessionService::write_project_file`, so the conflict banner,
encoding handling and the agent's view of the file stay as they are.

Limits to state in the docs: needs a Chrome on the machine (as the browser
tools do), `<select>` popups and file choosers are not painted by the
screencast (draw.io mostly uses its own menus), and typing has screencast
latency.

#### 3c. Later: WASM renderers

A renderer compiled to WASM (WASI, `wasmtime`) that turns text into SVG or
Markdown in-process, sandboxed and portable, without a command on the
user's machine. Only worth it once command renderers show demand for
"works everywhere without installing mmdc".

### 4. Provider templates

Today a provider is a hand-written entry in `providers.json` and its
models are picked in `model-selection.json`. A template prefills both.
`providers/<id>.json`:

```jsonc
{
  "id": "acme-gateway",
  "label": "ACME LLM Gateway",
  "provider": "openai-responses",            // an existing LlmApiClientKind family
  "config": {
    "base_url": "https://llm.acme.example/v1",
    "api_key": { "$field": "api_key" }       // filled in by the user
  },
  "fields": {
    "api_key": { "label": "API key", "secret": true, "help": "From https://llm.acme.example/keys" },
    "region": { "label": "Region", "choices": ["eu", "us"], "default": "eu" }
  },
  "models": [                                // offered in model-selection.json
    { "id": "gpt-5.5", "name": "GPT-5.5 (ACME)", "default_effort": "medium" },
    { "id": "acme-coder-2", "name": "ACME Coder", "behaves_like": "gpt-5.5" }
  ],
  "catalog": "catalog.json"                  // capability entries for models the built-in catalog lacks
}
```

*Settings → Providers → Add* lists templates from enabled plugins next to
the built-in provider types. Filling the form writes a provider entry that
remembers its template:

```json
"acme": { "label": "ACME LLM Gateway", "template": "diagrams/acme-gateway", "values": { "region": "eu" } }
```

A linked entry follows template updates (a new base URL, new models); the
user can *Detach* it into a plain entry. Catalog entries join
`llm::capabilities` resolution after the built-in catalog, so capabilities
of gateway-specific model IDs no longer need `behaves_like` hacks in user
config. Secret fields are written to the secret store (see below), not into
`providers.json`.

Built-in templates (OpenRouter, Ollama on localhost, Vertex, AI Core) could
move into a bundled system plugin through the same mechanism.

### 5. Managed providers

A managed provider is a template a team controls: the user logs in, but
never sees or pastes a key, cannot point it elsewhere, and gets the models
the team allows. Security rules:

- **No secrets in the repository.** The manifest names how to obtain a
  credential, never the credential.
- **Short-lived credentials.** Tokens with an expiry, refreshed before
  they run out; a revoked user loses access without touching their machine.
- **Locked fields.** `base_url` and similar fields come from the manifest
  and are not editable; the model list can be fixed (`"models": "locked"`).
- **Signed manifests.** A managed provider only loads from a plugin whose
  commit is signed by a key the user (or an admin-provided policy file)
  trusts, so a pushed change to the repository cannot redirect traffic.

Credential sources (one per provider):

```jsonc
"managedProviders": [{
  "template": "acme-gateway",
  "credential": {
    // 1. OAuth 2.0 / OIDC with PKCE against the company IdP (browser login),
    //    the flow llm::codex_auth already implements for ChatGPT logins:
    "oauth": {
      "issuer": "https://login.acme.example",
      "client_id": "code-assistant",
      "scopes": ["llm.gateway"],
      "audience": "https://llm.acme.example"
    }
    // 2. or a credential helper, like git's or AWS's credential_process:
    //    "command": ["acme-cli", "llm-token", "--json"]  → {"token": "...", "expires_at": "..."}
    // 3. or the device authorization grant, for machines without a browser
  },
  "lock": ["base_url"],
  "models": "locked",
  "policy": { "allow_custom_config": false }
}]
```

Where the pieces land:

- **Secret store.** A `SecretStore` seam in `llm` (OS keychain via the
  `keyring` crate, file fallback with 0600 permissions). `CodexTokenStorage`
  is the model: `ProvidersJsonTokenStorage` stays for existing setups, the
  keychain becomes the default for new secret fields. This also improves
  plain providers, whose API keys sit in `providers.json` today.
- **Auth seam.** A `CredentialSource` trait (`token() -> (String, Expiry)`)
  with OAuth, helper-command and static-key implementations; the factory
  asks it per request (cached until shortly before expiry), as
  `CodexAuthProvider` does now. An expired login shows like an MCP server
  that needs a login: a *Log in* button in the model picker, re-auth on the
  next request.
- **Admin policy.** An optional `managed-plugins.json` in a system location
  (`/etc/code-assistant/`, `/Library/Application Support/…`,
  `%ProgramData%`), deployable by MDM, that pre-installs plugins, pins their
  commits or signing keys, and can forbid unmanaged providers. Absent on
  personal machines.

### 6. Hooks

User-defined commands at points of the agent loop:

```json
{
  "hooks": {
    "PreToolUse":  [{ "matcher": "execute_command", "command": "${PLUGIN_ROOT}/hooks/guard.sh" }],
    "PostToolUse": [{ "matcher": "edit|write_file|replace_in_file", "command": "${PLUGIN_ROOT}/hooks/lint.sh" }],
    "SessionStart": [{ "command": "${PLUGIN_ROOT}/hooks/context.sh" }],
    "UserPromptSubmit": [], "Stop": []
  }
}
```

The hook gets a JSON event on stdin and answers with a JSON decision
(allow, deny with a reason the model sees, extra context to inject) or by
exit code. Mapping: `PreToolUse` → a `ToolInterceptor` in
`agent_core::hooks` that can veto or rewrite input; `PostToolUse` → an
interceptor appending to the result; `SessionStart`/`UserPromptSubmit` →
an iteration hook adding context; `Stop` → can ask the agent to continue.
`plugins::default_hooks()` becomes a builder that appends a
`CommandHookAdapter` per enabled plugin. Hooks run with the user's rights,
so they count as the `process` capability.

### 7. Slash commands (prompt templates)

`commands/<name>.md`: a Markdown prompt with optional frontmatter
(`description`, `argument-hint`, `model`), inserted by typing
`/<name> args` in the composer, `$ARGUMENTS` replaced. Needs a small composer feature
(completion popover for `/`), useful without plugins too
(`.agents/commands/` in a project).

### 8. Agent profiles

`agents/<name>.md`: a named sub-agent (system prompt addition, allowed
tools by tag, default model and effort) that `spawn_agent` can be asked to
use by name. Builds on the sub-agent feature.

## More suggestions

Smaller contribution types that fit the same manifest, roughly by value:

| Contribution | What it adds | Seam |
|---|---|---|
| Formatter presets | `formatters.json`: format-on-save commands per glob (`rustfmt`, `prettier`, `black`) | `projects.json` format-on-save (`docs/format-on-save-feature.md`) |
| Model catalog entries | capability data for models (context, effort levels) without a provider | `llm::capabilities` catalog lookup |
| Tool output renderers | render an MCP tool's result in its tool card with a file renderer (a returned diagram, a table) | tool cards + renderer table |
| Themes | gpui-component theme JSON | settings appearance |
| Browser profiles | prepared browser profiles / login hints for sites a team uses | `docs/browser-agency-plan.md` |
| Sandbox profiles | allowed paths and hosts for command execution | `sandbox` crate |
| Session templates | a starting prompt, tool set, model and plan for recurring tasks ("release", "dependency update") | new session dialog |
| Voice personas | instructions and voice for the voice agent | `voice.json` |
| Status items | a command whose short output shows in the status bar (CI state, ticket) | GPUI status area |
| Line comment actions | extra actions on a selection in the Files view ("explain", "open ticket") | selection pill |
| Project scaffolds | templates for new projects, shown in the project dialog | project dialog |

Recommended first wave: skills, MCP servers, command renderers, provider
templates. They need the least new UI and cover most of what the existing
ecosystem already ships.

## Enabling and scoping

Three levels, as for built-in tool groups and MCP servers:

1. **Global**: `enabled` in `plugins.json`, *Settings → Plugins*.
2. **Per project**: remembered last choice, stored with the project.
3. **Per session**: the *Tools* menu lists plugins with their contributions
   (servers, skills); switching a plugin off there hides its tools and
   skills from that session via a `plugin:<name>` tag on every contributed
   tool (the `toolset:<id>` mechanism) and a filter in the skills catalog.

Renderers and provider templates are app-level (they do not change what the
model sees) and follow the global switch only.

## Security model

Plugins are code from the internet; the design keeps that visible rather
than pretending to sandbox everything.

- **Capabilities, shown at install and on widening.** `process` (commands,
  stdio MCP servers, hooks, command renderers), `network:<hosts>` (web
  renderers' reachable hosts, remote MCP servers), `providers` (adds
  provider templates; managed providers additionally need a signature). An
  update that adds a capability asks again; an update within the granted
  set only shows the changelog.
- **Pinned commits.** Nothing changes without an update the user confirms.
  `plugins.json` stores the commit, not just the ref.
- **Honesty about processes.** A launched process runs with the user's
  rights. The sandbox crate can confine it later (a per-plugin sandbox
  profile); until then the install dialog says so plainly.
- **Web renderers are confined.** One Chrome profile per plugin, never the
  agent's or the user's browser profiles; one origin per plugin; network
  restricted to the declared hosts by CDP request interception
  (`Fetch.enable` with a deny-by-default pattern); no file system access
  beyond the file shown, passed as text through the host API.
- **No access to secrets.** Plugins never read `providers.json` or the
  secret store; a managed provider's credential flows from the IdP or helper
  to the request, not through plugin code.
- **Project files need trust.** `.agents/plugins.json` follows the
  `.mcp.json` trust flow; a declared but untrusted plugin is not loaded.
- **Review is easy.** *Settings → Plugins* shows a plugin's files, its
  manifest and the diff of a pending update, and links to the repository.

## Architecture

```
code_assistant (binary)    CLI: plugin add/list/update/remove/marketplace
ui_gpui                    Settings → Plugins; renderer table built at runtime;
                           web renderer view (screencast + host bridge)
code_assistant_core
  plugin_host::store       clone/fetch/pin, plugins.json, marketplaces, trust
  plugin_host::manifest    plugin.json + marketplace.json parsing, validation
  plugin_host::catalog     enabled plugins → contributions, app-scoped
                           UiEvent::PluginsChanged
  skills::loader           + Plugin scope
  tools::ConfigToolRegistry + plugin MCP servers, plugin lock in fingerprint
  plugins (hooks)          default_hooks() → builder + CommandHookAdapter
  session::SessionService  list/install/update/enable plugins, provider templates
llm                        SecretStore, CredentialSource, template catalog entries
web                        renderer tabs: per-plugin profile, request filtering,
                           host bridge bindings, loopback static server
```

`PluginCatalog` is the single place contributions come from; each consumer
asks it at the moment it already rebuilds (the tool registry per run, the
skills catalog per run, the renderer table when the catalog changes), so no
consumer needs a restart. Removal is ownership-driven like MCP connections:
a checkout is deleted once no live registry or view references it.

## Order of work

1. **Store and manifest.** Install from git URL and local folder, pin,
   update, remove; `plugins.json`; CLI and a minimal *Settings → Plugins*
   list. Contributions: skills and MCP servers (both have loaders already).
2. **Marketplaces** and the *Browse* tab; project `.agents/plugins.json`
   with the trust prompt.
3. **Renderer table at runtime** and command renderers (Mermaid as the
   showcase plugin).
4. **Provider templates** and the `SecretStore`, with the built-in provider
   types moved behind templates where it simplifies the settings form.
5. **Web renderers** on the screencast machinery; a draw.io plugin with a
   bundled and a remote variant.
6. **Managed providers**: `CredentialSource` (OAuth PKCE, helper command),
   locked fields, signature check, admin policy file.
7. **Hooks, slash commands, agent profiles**, then the smaller types from
   *More suggestions* as demand shows.

## Open questions

- **Signing.** Git commit signatures (GPG/SSH, verified against pinned
  keys) or a detached signature over the manifest in a release asset?
  Commit signatures need no extra tooling for authors.
- **Web renderer latency.** Is a screencast good enough for drawing in
  draw.io, or does that renderer wait for the CEF stage? A prototype with
  the existing browser panel answers it quickly.
- **Plugin settings.** Plugins will want user settings (a Kroki URL, a
  default region). A `settings` schema in the manifest rendered as a form,
  values in `data/<plugin>/settings.json`, exposed as `${setting:name}`?
- **Version ranges for dependencies** between plugins (a renderer plugin
  needing a server plugin): skip in v1 and keep plugins self-contained.
