# Browser tools

The agent drives a real Chromium through the browser tools in
`crates/code_assistant_core/src/tools/impls/browser/`, on top of the
`web` crate (`chromiumoxide`, i.e. the Chrome DevTools Protocol — not
Playwright). Their shape follows the computer-use style browser tools models
are trained on: read the page as an accessibility tree, act on elements by
`ref_N` or on screenshot coordinates, and look only when needed.

The history and the login design are in `docs/browser-agency-plan.md`.

## Tools

| Tool | Does | Read-only |
|---|---|---|
| `browser_navigate` | load a URL, or `back`/`forward`; opens the browser | yes |
| `browser_read_page` | accessibility tree, `- role "name" [ref_N] state` | yes |
| `browser_find` | tree lines matching a query (max 20) | yes |
| `browser_get_page_text` | main/article text, else the body | yes |
| `browser_computer` | click/type/key/scroll/hover/drag/wait, screenshot, zoom, record | no |
| `browser_form_input` | set a select, checkbox or field by ref | no |
| `browser_javascript` | run JS with REPL semantics | no |
| `browser_read_console_messages` | console, exceptions, browser log | yes |
| `browser_read_network_requests` | requests, or one response body | yes |
| `browser_resize_window` | viewport presets, phone emulation, color scheme | no |
| `browser_tabs_context` / `_create` / `_select` / `_close` | tabs | context only |
| `browser_batch` | several of the above in one call | no |
| `browser_close` | close a profile's browser | no |
| `browser_login` | human-in-the-loop login on a persistent profile | yes |
| `browser_profiles` | persistent profiles and their last login | yes |

Every tool takes an optional `profile` (default: a throwaway browser) and,
where it acts on a page, an optional `tab_id` (default: the active tab).
Together the definitions are about 12.8k characters (~3.3k tokens).

## Concepts

- **Refs.** `browser_read_page`/`browser_find` give each element a `ref_N`,
  mapped to its DOM backend node id (`web::ax_tree::RefMap`). A ref stays the
  same across reads of one document and goes stale on navigation; using a stale
  ref is a clear error asking to read the page again.
- **Coordinate frame.** `browser_computer` coordinates are pixels of the most
  recent screenshot, which reports its size. A screenshot taken with `scale`
  (or shrunk to fit the 1568 px image limit) changes the frame; `zoom` does
  not. The headless viewport is 1280×800 at device scale 1.
- **Observe on demand.** Actions return one line plus notes, no screenshot.
  Notes cover what happened around the action: dialogs answered, tabs the page
  opened, a navigation it caused.
- **Dialogs.** `alert`/`beforeunload` are accepted, `confirm`/`prompt`
  dismissed unless `browser_computer` gets `accept_dialogs: true`. An open
  dialog freezes the renderer, so it is never left open.
- **Timeouts.** Each verb is bounded (15 s, navigation 30 s;
  `web::BrowserTimeouts`); chromiumoxide's own timeout does not hold on a
  stuck renderer. A navigation whose `load` never fires is shown as far as it
  got, with a note.
- **Tabs.** Pages the site opens (`target=_blank`, `window.open`) are adopted
  as tabs without becoming active; Chrome's initial blank tab is ignored.
  Closing the last tab closes the browser.
- **Console and network** are collected per tab in the background (last 500
  each), so they can be read after the fact.
- **Real time.** The page keeps running between calls, also while the model
  thinks. Timing-sensitive input goes in one `browser_batch`, whose steps run
  back to back (`key_down w`, `record`, `key_up w`).
- **Recording.** `browser_computer` `record` films `duration` seconds (max 5)
  and returns one contact sheet: `frames` cells (default 9, 3×3) evenly spread
  from start to end, left to right, top to bottom, each labelled `#n +0.25s`.
  The sheet starts from a screenshot of the page as the recording begins;
  after that, frames come from a CDP screencast (`Page.startScreencast`),
  which only sends a frame when the page repaints. A cell shows the last
  frame painted by its moment. Cells that look like the one before are listed
  as unchanged in the text and dimmed in the sheet. The sheet stays within the
  1568 px edge limit; `scale` shrinks its cells. It does not change the
  coordinate frame.
- **Headless window.** A screencast frame shows the browser window, not the
  emulated viewport, and headless Chrome reserves part of its window for
  browser UI (143 px of its height on macOS; its default window is 800×600).
  So a headless browser gets a window 200 px taller than its viewport,
  emulating a larger viewport grows it, frames are cut to the viewport, and a
  frame that still shows less than the viewport grows the window by what is
  missing (that frame is dropped).

## Engine (`web` crate)

- `BrowserSession` — one launched browser and its tabs.
- `Tab` (`tab.rs`) — one page and its state: refs, screenshot frame, dialogs,
  console/network log (`page_log.rs`), and every page verb.
- `recording.rs` — contact sheets: which frame each cell shows, layout, and
  a built-in 5×7 bitmap font for the labels (pure, unit-tested).
- `ax_tree.rs` — the accessibility tree, read through a raw CDP command with
  lenient structs, and its rendering (pure, unit-tested).
