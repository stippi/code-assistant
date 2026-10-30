# File Renderers

The Files view of the right panel shows a file in the text editor. Some
files read better another way: Markdown rendered as in the chat, SVG
files drawn, diagrams in their editor. A *renderer* is such a way: built
in (Markdown, SVG) or contributed by a plugin: a command that turns a file
into an SVG or an image, or a web editor that edits the file in place
([plugin renderers](#plugin-renderers)).
When the open file has one or more, a dropdown in the viewer's header
switches between them and **Text**, which every file has.

## Behavior

- A file shows with the first renderer that takes it, plugin renderers
  before the built-in ones; files no renderer takes show as text, without a
  dropdown.
- Picking an entry in the dropdown holds for files of the same kind (the
  lowercase extension, or the name of a file without one) and is remembered
  per session (`FilesViewState::renderers` in `ui_state`). Picking **Text**
  for `.md` shows Markdown files as text from then on. A pick that cannot
  show a file of that kind (one made on `flow.diagram.svg` for a renderer
  of `*.diagram.svg` files, when `logo.svg` opens; or a renderer since
  removed) shows the file with its first renderer instead.
- The text editor still holds the file while a renderer shows it: loading,
  reloading on disk changes and saving stay in `FileViewer`. A renderer gets
  the editor's text, so a reload shows in it at once.
- Opening a file at a line (from a tool card) and revealing a comment (from
  the composer's list) go to the renderer when one shows the file. A
  renderer that cannot show lines or comments (SVG) says so, and the file
  opens in the text editor for that request; the remembered pick stays.

## Markdown

`file_renderers/markdown.rs` renders the file with gpui-component's
`TextView` (scrollable, selectable), as the chat renders messages.

- **Selection pill.** Releasing the mouse over a selection shows the pill
  with **Copy** and **Comment**, as in the editor and the chat.
  Cmd/Ctrl-Shift-M comments on the selection too, also one without a pill
  (select all, or a pill dismissed by scrolling). Copy copies the Markdown
  source of a selection across styles and the plain text of one within a
  style (`SelectionFormat::SourceAcrossStyles`).
- **Comments are line comments.** `TextViewState::selected_source_range`
  maps the selection back to the Markdown source; the comment covers the
  whole source lines it touches and carries them as its excerpt. It is an
  ordinary comment on the file: the text editor marks the same lines, it is
  re-anchored like any other, and the model gets path and line numbers. A
  selection on lines that already have a comment opens that comment.
  While a reload's parse is pending, a selection still indexes the old
  text and offers no comment.
- **Marks.** Commented passages are tinted with range highlights
  (`RenderedText::range_for_source`), computed again whenever a parse lands
  or the comments change.
- **Lines.** Going to a line scrolls to it; a line that renders nothing
  (blank, a code fence) scrolls to the first text after it.
- **Links.** Relative links, and links from the project root (`/docs/a.md`,
  as GitHub reads them), open the linked file in the Files view
  (`OpenFileBus`); links with a scheme open in the browser; anchors within
  the page do nothing.

Not done yet: relative images are not resolved against the file's folder,
and fenced code blocks are not highlighted (neither is the chat's).

## SVG

`file_renderers/svg.rs` draws an `.svg` file (listed as **Image**), fitted
into the view and redrawn from the editor's text. GPUI rasterizes an SVG
image once at its own size, which blurs an icon shown large, so the view
rasterizes it itself with resvg for the size it is shown at times the
display's scale, again when that size changes; parsing and rasterizing run
in the background. Most drawings expect a white page, so it starts on
white; a button in the corner switches to the theme's background.
`.drawio.svg` files show their diagram this way (labels drawn as HTML
`foreignObject` show as their plain-text fallback, if any). It takes no
comments.

Parsing and fonts are shared with the `view_images` tool
(`tools_core::image::{parse_svg, svg_to_png}`), which hands SVG files to
the model as PNG on white, the longer edge at least 1024 px (an icon large
enough to make out) and at most `MAX_IMAGE_EDGE`. System fonts load once,
and only for drawings with text.

## Plugin renderers

Renderers beyond the built-in ones come with plugins
([plugins.md](plugins.md)): a plugin's manifest lists them under
`fileRenderers`, nothing is configured in the settings. Installing,
switching or removing a plugin in *Settings → Plugins* changes what the
Files view offers at once (`shared::custom_renderers::reload`; a plugin
changed with the command line is picked up at the next start). For
renderers of your own, make a plugin in a local folder and install that:
it is linked, so edits to its manifest show without reinstalling.

```json
{
  "name": "diagrams",
  "fileRenderers": [
    {
      "label": "Graphviz",
      "files": ["*.dot", "*.gv"],
      "kind": "command",
      "command": "dot -Tsvg",
      "output": "svg"
    },
    {
      "label": "Diagrams",
      "files": ["*.diagram"],
      "kind": "web",
      "url": "http://localhost:8080/?embed=1&proto=json&dark={dark}",
      "scripts": ["editor/shapes.js"],
      "start": { "command": "scripts/serve-editor.sh" }
    }
  ]
}
```

Every renderer has:

- **`label`**, its name in the dropdown, unique within the plugin; picks
  are remembered as `plugin:<plugin>/<label>`.
- **`files`**, patterns for file names (not paths): `*` and `?` are
  wildcards, case is ignored. `*.diagram.svg` takes those SVG files only,
  ahead of the built-in SVG view.
- **`kind`**: `command` or `web`.
- **`start`** (optional): a command that starts a server the renderer
  needs, see [Servers](#servers).

`${PLUGIN_ROOT}` in a command, an address or a script entry is the
plugin's folder. A manifest whose renderers lack what their kind needs (a
`command`, an http(s) `url`) or carry the other kind's fields does not
load.

A renderer that cannot show lines or comments (both kinds) leaves those
requests to the text editor, as for SVG.

### Commands

- **`command`** is a shell command line (`sh -c`, on Windows `cmd /C`) run
  in the project's root folder. `{file}` is the file's path, `{output}` a
  path in a temporary folder to write the result to; without `{output}` the
  result is read from standard output. The file's text is also on standard
  input, for tools that read it there (`dot -Tsvg`). Paths are quoted.
- **`output`** is what the command produces: `svg` (the default), drawn by
  the SVG view, sharp at any size; or `image` (alias `png`), any raster
  image the `image` crate reads, fitted into the view on white, never
  scaled up.

How it runs (`shared/custom_renderers.rs`, view in
`file_renderers/command.rs`):

- The command runs when the file opens with the renderer and again when
  its text changes (a reload from disk), 400 ms after the last change; a run
  still going is stopped. The last output stays in view, marked
  *Updating…*, until the next arrives.
- When the editor's text differs from the file on disk (not saved yet),
  `{file}` is a copy with the same name in a temporary folder, so the
  command sees the current text.
- A run that fails (exit status, nothing written, output that is no image,
  60 s timeout) shows why, with the last lines of its error output.
- The command runs on the GPUI background executor and blocks one of its
  threads until it ends.

### Web editors

A web app that edits the file in place, shown in a webview over the view
(macOS only for now; elsewhere such renderers are not offered). View in
`file_renderers/web_editor.rs`.

- **`url`** is the editor's address, with whatever it needs to talk the
  embed protocol below. `{dark}` becomes `1` in a dark theme, else `0`.
- **`scripts`** are files injected into the editor, paths in the plugin's
  folder, for example plugins of the editor. Each runs once the editor's page has loaded; one that fails
  because what it uses is not there yet (a `ReferenceError` or
  `TypeError`) is tried again for up to 30 s, so it can wait for the
  editor's own hooks. A script that still fails is logged as a warning.
  Scripts are read when the editor opens.

**The embed protocol.** The editor runs in an iframe of a small page; they
talk JSON strings over `postMessage`:

1. The editor sends `{"event": "init"}` once it has started.
2. The page sends `{"action": "load", "xml": <the file's text>,
   "autosave": 1, "title": <file name>}`.
3. The editor sends each change as `{"event": "autosave", "xml": <text>}`
   (a save as `"save"`). The text goes into the text editor as an edit and
   is saved by its auto-save (1 s after the last change).

Text from outside (a reload after the file changed on disk) is loaded into
the editor again; the editor's own edits coming back are not.

**Native view.** The webview (`wry`, WebKit) is a native view over the
window, not part of GPUI's element tree (`shared/native_views.rs`):

- Its element moves it each time it is painted; a frame that does not paint
  it (another panel tab, the settings) hides it. The root view marks every
  native view unpainted as it renders, and `Window::on_next_frame` hides
  those still unpainted after the frame.
- It covers everything GPUI draws, popups too, so it is hidden while a
  select's or combobox's list is open (gpui-component's deferred popovers),
  such as the renderer dropdown above the file.
- A click into the editor clears GPUI's focus (a script in the editor's
  frame reports it), or GPUI's key bindings would take keys meant for the
  editor, such as Delete.
- The theme is picked when the editor opens.

### Servers

A renderer may need a server: a web editor served on localhost (a
self-hosted draw.io), or a command calling a local service. Its `start`
says how to start it:

```json
"start": {
  "command": "scripts/start-drawio.sh",
  "url": "http://localhost:8080/",
  "args": [], "env": {},
  "timeoutSeconds": 300
}
```

- **`url`** is where the server answers once it runs; a web editor's own
  `url` when left out, needed for a command renderer.
- **`command`** runs in the plugin's folder (a relative path is taken from
  there), in its own process group, with `PLUGIN_ROOT` and `SERVER_URL`
  set. It should stay in the foreground (no daemonizing).
- **`timeoutSeconds`** (default 120) is how long to wait for the server.

Opening a file with such a renderer shows "Starting the server of …"
(`file_renderers/server_gate.rs`) until something accepts connections at
the address's host and port: a server already running there (the user's
own, or a remote one) is used as it is; otherwise the command runs. A
command that exits first, or a server that does not answer in time, shows
why with the last lines of its output and a *Try again* button. Then the
renderer's own view takes over. A started server is shared by all views of
that renderer, stopped (its whole process group) 5 minutes after the last
one closed, and stopped when the app quits (`plugin_host::server`).

## Adding a built-in renderer

No plugin system: a built-in renderer is compiled in and listed in a static
table.

1. A module under `crates/ui_gpui/src/main_screen/right_panel/file_renderers/`
   with a view implementing `RendererView`:

   ```rust
   pub trait RendererView:
       Render + EventEmitter<CommentChange> + EventEmitter<TextEdit> + Sized
   {
       fn set_text(&mut self, text: &str, cx: &mut Context<Self>);
       // Optional; the defaults do nothing, and the reveals return false
       // (cannot show it), which opens the file in the editor instead:
       fn set_comments(&mut self, comments: &[LineComment], cx: &mut Context<Self>);
       fn reveal_line(&mut self, line: usize, window: &mut Window, cx: &mut Context<Self>) -> bool;
       fn reveal_comment(&mut self, comment: LineComment, window: &mut Window, cx: &mut Context<Self>) -> bool;
       fn start_comment(&mut self, window: &mut Window, cx: &mut Context<Self>);
   }
   ```

2. A `BuiltIn` entry in `BUILT_IN`: a stable `id` (stored in
   `ui_state`), the dropdown `label`, `accepts(path)` and `create(file,
   root, window, cx)`, which returns the view boxed as a `RendererHandle` (blanket
   implemented for `Entity<V: RendererView>`).

Comment changes the view emits reach the composer through the viewer. A
view that edits the file emits `TextEdit` with the new text; the viewer puts
it into the text editor, which saves it.

### Where this would grow

- **Double extensions.** Picks are remembered by the last extension, so a
  pick for `.diagram.svg` is one for `.svg` too (and falls back where it
  does not apply) unless `file_kind` learns double extensions.
- **Web editors elsewhere.** `wry` has webviews on Windows and Linux too;
  the native view handling would need trying there. A web editor of SVG
  files with an embedded source would also need the `export` action of
  the protocol for saving.
- **Offline web editors.** An editor served from a local folder through a
  custom protocol (`wry::WebViewBuilder::with_custom_protocol`) instead of
  an address.
- **Binary files** (images, PDFs, `.vsdx`). `FileViewer` only loads text
  today and shows a notice for binary files. A renderer for them would need
  a flag on `FileRenderer` saying it reads the file itself, so the viewer
  skips the text load and lets it show a file the editor cannot; custom
  renderers, which read `{file}` themselves, could set it.
- **Other outputs.** A custom renderer producing Markdown (`pandoc`,
  `nbconvert`) could show it with the Markdown view, without comments, as
  its lines are not the file's. HTML could be shown by a web view.
