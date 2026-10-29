# Plugins

A plugin is a git repository (or a local folder) with a manifest at
`.code-assistant-plugin/plugin.json`. Installing it adds what the manifest
contributes:

- **Provider templates**: prefilled providers offered when adding one.
- **MCP servers**: added to every session while the plugin is on, not
  editable in the settings.
- **File viewers**: web pages that show, and may edit, files in the Files
  view, with an optional script that starts their server.

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
commit and whether it is on. Plugins can start programs (MCP servers, viewer
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

  "fileViewers": [{
    "id": "drawio",                     // unique in the plugin
    "label": "draw.io",                 // name in the Files view's dropdown
    "files": ["*.drawio", "*.drawio.svg"],  // file name patterns, any case
    "url": "http://localhost:8080/?offline=1&https=0",
    "protocol": "drawio",               // host-api (default) | drawio | none
    "start": {                          // optional
      "command": "scripts/start-drawio.sh",  // relative: from the plugin root
      "args": [], "env": {},
      "timeoutSeconds": 300             // default 120
    }
  }]
}
```

Parsing is strict: an unknown key, a bad name, a duplicate id, a viewer
without patterns or with a non-http(s) URL, or an MCP entry `.mcp.json`
would reject makes the manifest invalid. An installed plugin whose manifest
does not load is listed with the error and contributes nothing.

`${PLUGIN_ROOT}` stands for the plugin's folder wherever a path goes: MCP
commands, arguments, URLs, env and header values; start commands, their
arguments and env.

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

## File viewers

A viewer is a web page in the Files view. The page runs in a headless
Chrome tab of its own and is drawn from the tab's screencast, as the
browser panel draws the agent's browser (`docs/browser-panel-plan.md`, which
also explains why there is no native web view). Mouse, wheel and keyboard go
to the page (always: nobody else acts on it); copy and paste use the system
clipboard. The page is laid out for the view's size.

Opening a file with a viewer (the first renderer taking a `.drawio` file,
or picked in the dropdown):

1. `server::ensure`: if something accepts connections at the URL's host and
   port, it is used. Otherwise the start command runs in the plugin folder
   (in its own process group, `PLUGIN_ROOT` and `VIEWER_URL` set) and the
   viewer waits until the URL answers, up to `timeoutSeconds`; a command
   that exits first is reported with its last output lines. A started
   server is shared by all viewers of that plugin viewer, stops (the whole
   group) 5 minutes after the last one closed, and stops when the app quits.
   Without a start command, a URL nobody answers on is an error with a
   *Try again* button.
2. A browser tab opens, a page script for the protocol is added before the
   page's own scripts, and the page loads. `{path}` and `{relative_path}` in
   the URL are replaced by the file's paths (URL-encoded).
3. The file's text goes to the page whenever the page's script is ready
   (also after the page reloads) and whenever the file changes, on disk or
   in the editor, unless the page has that text already. Text the page
   writes comes back as a `RendererEdit`: the file viewer puts it into the
   editor and saves it like typing (auto-save, conflict banner and all).

### Protocols

- **`host-api`** (default): the page uses `window.codeAssistant`:

  ```js
  codeAssistant.onText(text => render(text));  // at once, and on every change
  codeAssistant.setText(newText);               // write the file
  codeAssistant.path; codeAssistant.relativePath;
  ```

- **`drawio`**: draw.io's embed protocol. The app adds `embed=1`,
  `proto=json`, `spin=1` and hides draw.io's save and exit buttons
  (`saveAndExit=0&noSaveBtn=1&noExitBtn=1`) unless the URL sets them. Embedded
  in a frame, draw.io talks to its parent with `postMessage`; here it is the
  top window and its own parent, so the page script takes draw.io's events
  off `window.postMessage` before they are delivered and posts the app's
  actions to the window, where draw.io accepts them as its parent's. The
  diagram loads with autosave on; every autosave writes the file. A
  `.drawio.svg` file loads from its `content` attribute and is written as
  draw.io's `xmlsvg` export, so it stays both an image and an editable
  diagram.
- **`none`**: no text exchange; the page shows the file itself (from
  `{path}` in the URL, say). Edits in the page do not reach the file view.

### Where it lives

| Part | Code |
|---|---|
| Page bridge (init scripts, bound functions) | `web::bridge` (`Tab::add_init_script`, `Tab::bind`) |
| Server start/stop | `plugin_host::viewer::server` |
| Opening, text exchange, page scripts | `plugin_host::viewer` (`open`, `host_api.js`, `drawio.js`) |
| Service entry | `SessionService::open_file_viewer` |
| GPUI view | `file_renderers/web.rs` (feature `browser-panel`) |
| Renderer table | `file_renderers::Renderer` (built-ins, then plugin viewers) |

A pick in the dropdown is remembered per kind of file (last extension), as
for built-in renderers. A pick that does not take a file falls back to the
first renderer that does, so picking draw.io for `flow.drawio.svg` does not
make `logo.svg` open as text.

### Limits

- Needs Chrome on the machine, like the browser tools.
- `<select>` popups, file choosers and other native browser UI are not
  painted into the screencast; input methods (IME composition) are not
  forwarded yet.
- The page is drawn at one pixel per CSS pixel, so it is softer than the
  rest of the app on a high-density display.
- Started servers are stopped when the app quits normally; after a crash a
  server keeps running until stopped by hand.

## Tests

- `plugin_host::{manifest, store, catalog}`: parsing, installing from a
  local git repository and a linked folder, updating, removing, switching,
  MCP server localization.
- `tools::mcp::tests::resolve_mcp_server_includes_plugin_servers`.
- `plugin_host::viewer::server`: a running server is used, a start command
  runs until its URL answers and stops on drop, a failing one is reported.
- `plugin_host::viewer::tests::viewer_pages_exchange_the_text` (Chrome): a
  host-api page and a page speaking draw.io's protocol get the text and
  write edits back.
- `plugin_host::viewer::tests::real_drawio_loads_and_writes_back`
  (ignored, needs Chrome and a draw.io): set `DRAWIO_URL` to a draw.io, e.g.
  `http://localhost:8080/?offline=1&https=0` with `jgraph/drawio` running,
  and run it with `--ignored`.
- `web::tests::init_scripts_and_bindings_talk_to_the_embedder` (Chrome).
- GPUI: `file_renderers::tests` (plugin viewers in the table, picks),
  `file_viewer::tests::renderer_edits_go_into_the_editor`.
