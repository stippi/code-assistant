# Plugins

A plugin is a git repository (or a local folder) with a manifest at
`.code-assistant-plugin/plugin.json`, or a folder of a marketplace: a
repository listing several plugins (see [Marketplaces](#marketplaces)).
Installing it adds what the manifest contributes:

- **Provider templates**: prefilled providers offered when adding one.
- **MCP servers**: added to every session while the plugin is on, not
  editable in the settings.
- **File renderers**: ways to show files in the Files view: a command that
  turns a file into an image, or a web editor that edits it in place, with
  an optional script that starts a server they need.

The ideas behind it, and what may come later, are in
[plugins-concept.md](plugins-concept.md). A working example is in
[`examples/plugin`](../examples/plugin).

## Installing

From *Settings → Plugins* (paste a source, *Install*) or the command line:

```bash
code-assistant plugin add https://github.com/acme/code-assistant-plugin
code-assistant plugin add acme/code-assistant-plugin#v1.2   # GitHub, at a tag or branch
code-assistant plugin add ~/dev/my-plugin                    # a local folder, linked
code-assistant plugin add acme/plugins                       # a marketplace: added, not installed
code-assistant plugin add diagrams@acme                      # a plugin of an added marketplace
code-assistant plugin list
code-assistant plugin update [name]                          # no name: all plugins and marketplaces
code-assistant plugin disable|enable|remove <name>
code-assistant plugin marketplace add|list|update|remove …
```

A source is an existing folder, a git URL (`https://`, `ssh://`, `git@…`,
`file://`) or GitHub's `owner/repo`; a git source may end in `#ref` (a
branch, tag or commit). When the folder or repository holds a marketplace
manifest, adding it adds the marketplace (`PluginStore::add` looks).

- **Git plugins** are cloned shallowly with the user's own `git` (SSH keys,
  credential helpers and proxies work for private repositories, prompts are
  off) into `<config_dir>/plugins/<name>/`. The commit is recorded. *Update*
  clones again into `plugins/.staging/` and replaces the folder only once
  the new manifest checks out and still carries the same name.
- **Local folders** are used in place, not copied: editing the manifest
  shows at once, which is what developing a plugin wants. Removing the
  plugin keeps the folder.

`<config_dir>/plugins/plugins.json` records each plugin's source, folder,
commit and whether it is on. Plugins can start programs (MCP servers, renderer
servers), so the settings page says to install only plugins you trust.

## Marketplaces

A marketplace is a repository holding several plugins, each in a folder of
its own, listed in `.code-assistant-plugin/marketplace.json` at its root
(the idea of Claude Code's `.claude-plugin/marketplace.json`, in this app's
format):

```jsonc
{
  "name": "acme",                       // [a-z0-9-], at most 64, unique
  "description": "ACME's plugins",      // optional
  "owner": { "name": "ACME Platform Team", "email": "…", "url": "…" },  // optional
  "plugins": [
    { "name": "diagrams", "source": "./plugins/diagrams", "description": "…" },
    { "name": "gateway", "source": "./plugins/gateway", "version": "1.0.0" },
    { "name": "root-plugin", "source": "./" }   // a plugin at the root itself
  ]
}
```

- Adding a marketplace clones (or links) it into
  `<config_dir>/plugins/.marketplaces/<name>/` and records it in
  `plugins/marketplaces.json`; nothing is installed yet.
- Its plugins install as `<plugin>@<marketplace>`: *Settings → Plugins*
  lists every added marketplace's plugins with a switch (on installs and
  enables, off disables and keeps it installed).
- Each `source` is a folder inside the repository with a complete plugin
  (its own `plugin.json`, carrying the entry's name). Entries cannot point
  to other repositories or leave the repository: one clone, one commit and
  one review per marketplace, and one login for a private one.
- Installed plugins live in the marketplace's clone and record its commit.
  Updating a marketplace (or one of its plugins) fetches it once and points
  its plugins at their folders in the new commit; a plugin it no longer
  lists then fails to load and says so.
- Removing a plugin keeps the clone; removing the marketplace removes the
  plugins installed from it, and its clone (a linked folder is kept).
- A plugin at the repository root is listed with `"source": "./"`. A
  repository holding both manifests is added as the marketplace.

## Private repositories

Git runs as the user runs it, so it logs in the way the user's git does:
credential helpers (the macOS keychain, Git Credential Manager,
`gh auth setup-git`) for HTTPS, the SSH agent for SSH URLs. The app never
asks for or keeps passwords or tokens.

- In the app, git cannot prompt (`GIT_TERMINAL_PROMPT=0`). When a host
  refuses for want of a login — or answers "repository not found", as
  hosts do for private repositories — the clone fails with
  `GitLoginRequired`, which names the repository and says how to give git
  a login: log in once with git in a terminal so a credential helper keeps
  it, or load an SSH key into the agent.
- On a terminal, `code-assistant plugin …` lets git prompt when its helpers
  have no login (`PluginStore::interactive`); a helper that stores logins
  keeps the one typed there for the app and later updates.
- Updates fetch with the same login, so a private marketplace or plugin
  keeps updating as long as the helper holds it.

## The manifest

```jsonc
{
  "name": "acme",                       // [a-z0-9-], at most 64, unique
  "version": "1.0.0",                   // optional, shown in the list
  "description": "…",                   // optional
  "author": "…", "homepage": "…",       // optional

  "providerTemplates": [{
    "id": "gateway",                    // unique in the plugin
    "label": "ACME Gateway",            // provider label and picker entry
    "description": "…",                 // optional, shown under the picker
    "provider": "openai-responses",     // a provider type
    "config": { "base_url": "https://llm.acme.example/v1" },
    "models": ["gpt-5.5"]               // optional: offered in the model picker
  }],

  "mcpServers": {                       // the `.mcp.json` entry format
    "acme-tools": { "command": "${PLUGIN_ROOT}/bin/acme-mcp", "args": ["--stdio"] },
    "acme-remote": { "type": "http", "url": "https://mcp.acme.example/mcp",
                     "headers": { "Authorization": "Bearer ${ACME_TOKEN}" } }
  },

  "fileRenderers": [{
    "label": "Graphviz",                // name in the Files view's dropdown, unique
    "files": ["*.dot", "*.gv"],         // file name patterns, any case
    "kind": "command",                  // command | web
    "command": "dot -Tsvg",
    "output": "svg"                     // svg (default) | image
  }, {
    "label": "draw.io",
    "files": ["*.drawio"],
    "kind": "web",                      // macOS and Windows
    "url": "http://localhost:8080/?embed=1&proto=json&dark={dark}",
    "scripts": [],                      // injected into the editor, from the plugin folder
    "start": {                          // optional: a server the renderer needs
      "command": "scripts/start-drawio.sh",  // relative: from the plugin root
      "timeoutSeconds": 300,            // default 120; "url" defaults to the editor's
      "stop": { "command": "docker", "args": ["stop", "drawio"] }  // optional
    }
  }]
}
```

Parsing is strict: an unknown key, a bad name, a duplicate id or label, a
renderer without patterns or without what its kind needs (or with the other
kind's fields), or an MCP entry `.mcp.json` would reject makes the manifest
invalid. An installed plugin whose manifest
does not load is listed with the error and contributes nothing.

`${PLUGIN_ROOT}` stands for the plugin's folder wherever a path goes: MCP
commands, arguments, URLs, env and header values; renderer commands,
addresses and scripts; start commands, their arguments and env.

## Provider templates

*Settings → Providers → Add* shows a *Template* picker when enabled plugins
offer templates. Picking one fills in the label, the provider type and the
form's fields (base URL and the like); the API key is asked for, never part
of a template. Saving writes a normal `providers.json` entry: the template's
config fields the form does not show (extra headers, flags) are kept, what
the form shows wins. The template's `models` are switched on in
`model-selection.json`, the first becoming the default model when there is
none. The entry does not stay linked to the template.

(`settings_screen/providers_section.rs`, `ProviderFormHolder::prefill`.)

## MCP servers

`Catalog::mcp_servers` makes each plugin's servers independent of the working
directory: `${PLUGIN_ROOT}` replaced, a `./` or `../` command taken from the
plugin root, `PLUGIN_ROOT` set in a stdio server's environment, `${VAR}`s in
env and header values read from the environment (a server whose variable is
missing is left out and shows as failed). The servers join the configured
ones everywhere a server set is assembled:

| Where | What |
|---|---|
| `tools::mcp::register_configured_mcp_tools_in` | registry builds: global `mcp-servers.json`, then plugins, then a trusted project `.mcp.json`; a later source replaces a server of the same name |
| `tools::mcp::resolve_mcp_server` | logins and restarts of a single server |
| `tools::mcp_selection` | the session *Tools* menu (plugin servers are on by default) |
| `ConfigToolRegistry::fingerprint` | the catalog's fingerprint, so installing, updating or switching a plugin rebuilds the registry at the next run |

*Settings → MCP Servers* lists them under *From plugins*: read-only cards
(no switch, edit or delete) that still discover the server's tools and offer
the OAuth login. A server is switched per session in the input row, or with
its plugin.

## File renderers

The Files view offers the enabled plugins' renderers ahead of the built-in
ones, in the same dropdown; nothing is set up in the settings. The two kinds,
the draw.io-style embed protocol of web editors, and how servers are
started are described in
[file-renderers.md](file-renderers.md#plugin-renderers). In short:

- A **command** renderer runs a shell command in the project root that turns
  the file into an SVG or an image, again when the text changes.
- A **web** renderer shows a web editor in a native webview over the view
  (`wry`; macOS and Windows, not offered on Linux). It loads the
  file and saves every autosave back through the editor's auto-save.
- **`start`** runs the plugin's command when nothing answers at the
  renderer's address and waits for it; the server is shared, stops 5
  minutes after the last view closed, and stops when the app quits,
  through the plugin's `stop` command when it has one (`docker stop`).

| Part | Code |
|---|---|
| Manifest entries | `plugin_host::manifest::PluginFileRenderer` |
| Server start/stop | `plugin_host::server`, `SessionService::start_plugin_server` |
| Renderers as the Files view offers them | `ui_gpui::shared::custom_renderers` (`from_plugins`, `reload`) |
| Views | `file_renderers::{command, web_editor, server_gate}` |

## Tests

- `plugin_host::{manifest, store, catalog}`: parsing (renderer kinds),
  installing from a local git repository and a linked folder, updating,
  removing, switching, MCP server localization, renderer scripts and
  servers.
- `plugin_host::marketplace` and `store`: marketplace parsing (sources stay
  inside the repository), adding one from git or a folder, installing,
  updating (a plugin that moved) and removing its plugins, a plugin whose
  name differs from its entry, a server that answers 401 giving
  `GitLoginRequired`.
- `tools::mcp::tests::resolve_mcp_server_includes_plugin_servers`.
- `plugin_host::server`: a running server is used, a start command runs
  until its address answers and stops with its last lease, a failing one is
  reported.
- GPUI: `shared::custom_renderers` (plugin renderers localized, commands
  run), `file_renderers::tests` (plugin renderers first, picks fall back),
  `file_renderers::server_gate` (the view waits, then passes text and edits
  through), `files_view::tests::renderers_from_plugins_are_offered`.
