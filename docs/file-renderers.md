# File Renderers

The Files view of the right panel shows a file in the text editor. Some
files read better another way: Markdown rendered as in the chat, SVG
files drawn, later perhaps draw.io diagrams. A *renderer* is such a way.
When the open file has one or more, a dropdown in the viewer's header
switches between them and **Text**, which every file has.

## Behavior

- A file shows with the first renderer that takes it; files no renderer
  takes show as text, without a dropdown.
- Picking an entry in the dropdown holds for files of the same kind (the
  lowercase extension, or the name of a file without one) and is remembered
  per session (`FilesViewState::renderers` in `ui_state`). Picking **Text**
  for `.md` shows Markdown files as text from then on.
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

## Adding a renderer

A built-in renderer is compiled in and listed in a static table. Plugins
add web pages as renderers without compiling anything (see "Plugin file
viewers" below).

1. A module under `crates/ui_gpui/src/main_screen/right_panel/file_renderers/`
   with a view implementing `RendererView`:

   ```rust
   pub trait RendererView:
       Render + EventEmitter<CommentChange> + EventEmitter<RendererEdit> + Sized
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

2. A `FileRenderer` entry in `RENDERERS`: a stable `id` (stored in
   `ui_state`), the dropdown `label`, `accepts(path)` and `create(file,
   root, window, cx)`, which returns the view boxed as a `RendererHandle` (blanket
   implemented for `Entity<V: RendererView>`).

Comment changes the view emits reach the composer through the viewer. A
view that edits the file emits a `RendererEdit` with the new text; the
viewer puts it into the editor and saves it like typing (views that do not
edit only declare the emitter).

## Plugin file viewers

Enabled plugins add file viewers after the built-in renderers
(`file_renderers::Renderer::Plugin`, ids `<plugin>/<viewer>`, feature
`browser-panel`): a web page in a headless Chrome tab, drawn from its
screencast, that gets the file's text and can write it back — draw.io for
`.drawio` files, for example. See [plugins.md](plugins.md#file-viewers).

A pick that does not take the file being opened falls back to the first
renderer that does, so picking draw.io for a `.drawio.svg` file (remembered
for the kind `svg`) still shows other `.svg` files as images.

### Where this would grow

- **draw.io without a plugin.** A built-in view that draws the diagram
  from the file's XML, read-only (the `.drawio.svg` variant already shows
  through the SVG renderer). Editing in draw.io itself is what the plugin
  file viewers do.
- **Binary files** (images, PDFs). `FileViewer` only loads text today and
  shows a notice for binary files. A renderer for them would need a flag on
  `FileRenderer` saying it reads the file itself, so the viewer skips the
  text load and lets it show a file the editor cannot.
- **Command renderers from plugins.** A command that turns a file into SVG
  or HTML, its output shown (Mermaid, PlantUML, Graphviz); see
  [plugins-concept.md](plugins-concept.md).
