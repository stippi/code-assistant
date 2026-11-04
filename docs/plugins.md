# Plugins

A plugin is a git repository (or a local folder) with a manifest at
`.code-assistant-plugin/plugin.json`. Installing it adds what the manifest
contributes:

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
code-assistant plugin list
code-assistant plugin update [name]
code-assistant plugin disable|enable|remove <name>
```

A source is an existing folder, a git URL (`https://`, `ssh://`, `git@…`,
`file://`) or GitHub's `owner/repo`; a git source may end in `#ref` (a
branch, tag or commit).

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
    "kind": "web",                      // macOS for now
    "url": "http://localhost:8080/?embed=1&proto=json&dark={dark}",
    "scripts": [],                      // injected into the editor, from the plugin folder
    "start": {                          // optional: a server the renderer needs
      "command": "scripts/start-drawio.sh",  // relative: from the plugin root
      "timeoutSeconds": 300             // default 120; "url" defaults to the editor's
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
  (`wry`, macOS only for now; elsewhere it is not offered). It loads the
  file and saves every autosave back through the editor's auto-save.
- **`start`** runs the plugin's script when nothing answers at the
  renderer's address and waits for it; the server is shared, stops 5
  minutes after the last view closed, and stops when the app quits.

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
- `tools::mcp::tests::resolve_mcp_server_includes_plugin_servers`.
- `plugin_host::server`: a running server is used, a start command runs
  until its address answers and stops with its last lease, a failing one is
  reported.
- GPUI: `shared::custom_renderers` (plugin renderers localized, commands
  run), `file_renderers::tests` (plugin renderers first, picks fall back),
  `file_renderers::server_gate` (the view waits, then passes text and edits
  through), `files_view::tests::renderers_from_plugins_are_offered`.
