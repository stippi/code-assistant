# Browser Panel Plan

This document plans showing the agent's browser in the right sidebar of the
GPUI frontend, next to the diff review view, and how far the browser could
move from a separate Chrome process towards one built into the app.

The browser tools and the `web` engine are described in
`docs/browser-tools.md`; the login design in `docs/browser-agency-plan.md`.

## Summary

Two stages, sharing one UI:

1. **Live view over CDP screencast.** The headless Chrome launched by
   chromiumoxide stays as it is. Its frames are drawn in the panel; the user's
   mouse and keyboard go back over CDP. No change to the tool surface.
2. **Optional: embedded Chromium through CEF with off-screen rendering.** CEF
   opens a remote debugging port and chromiumoxide connects to it instead of
   launching a process. The tools stay the same; only the frame source and the
   browser launch change.

A native web view (WKWebView via `wry`) is not recommended (see below).

## Where we are today

- **Ownership.** `SessionInstance::browser_sessions`
  (`code_assistant_core/src/session/instance.rs`) holds one
  `web::BrowserSessionManager` per session, so browsers belong to the session,
  not to one agent run.
- **Frames exist.** `Tab::record` already uses `Page.startScreencast`
  (`web/src/tab.rs`), but only for recordings of at most 5 s.
- **Ephemeral browsers die with the turn.** The runner calls
  `close_ephemeral()` at the end of every turn
  (`code_assistant_core/src/agent/runner.rs`). With a panel, the browser would
  disappear after every agent reply.
- **The panel is a switcher already.** `RightPanelView`
  (`ui_gpui/src/main_screen/right_panel/mod.rs`) has a single variant,
  `Review`, but the type, its persistence (`as_str`/`from_str`) and the render
  match are built for more views.
- **Panel width.** `RIGHT_SIDEBAR_WIDTH` is 440 px; the agent's viewport is
  1280×800 (`web::DEFAULT_VIEWPORT`).
- **GPUI building blocks.** `img()` with `RenderImage` plus
  `Window::drop_image`, and on macOS the `surface()` element for a
  CVPixelBuffer/IOSurface.
- **Sub-agents** create their own `BrowserSessionManager`
  (`code_assistant_core/src/agent/sub_agent.rs`), outside the session
  instance, so their browsers would not show in a panel that reads the
  session's manager.

## Stage 1: live view over CDP screencast

### `web` crate (layer 0)

- **One screencast, several consumers.** A page session runs one screencast at
  a time, so `record` and a live view would interfere. `Tab` gets its own
  screencast pump that fans frames out:
  - the live view takes only the newest frame (`tokio::sync::watch`),
  - `record` collects every frame.

  The pump owns the frame acks and sets `maxWidth`/`maxHeight` from what the
  viewer asks for. `record` moves onto the pump.
- **Raw user input.** The existing verbs (`click`, `type`, ...) are shaped for
  the model. The panel needs raw forwarding:
  - mouse move/down/up with modifiers (`Input.dispatchMouseEvent`),
  - wheel,
  - key events with `key`, `code`, `windowsVirtualKeyCode`, `text`,
  - `Input.insertText` for IME,
  - on macOS the `commands` field (`copy`, `paste`, `selectAll`) for the
    clipboard.
- **Tab events.** URL, title, loading state, tab opened/closed, for the
  panel's address bar and tab strip.
- **Not in the screencast.** `<select>` popups, date pickers, file choosers,
  autofill and permission prompts are native UI and never painted into
  frames.
  - File choosers: `Page.setInterceptFileChooserDialog`.
  - `<select>`: a GPUI overlay fed from the element's options, or a JS
    replacement.
  - Dialogs are handled already.

### `code_assistant_core` (layer 3)

- **`SessionService` methods.** For example `browser_state(session)` and
  `browser_view(session, profile, tab) -> BrowserViewHandle`. The handle
  carries a newest-frame-wins frame channel and an input sender.
- **Frames stay out of the `EventStream`.** In the broadcast stream they would
  push other subscribers into `Lagged` and force snapshot resyncs. The stream
  carries metadata only: `BrowserOpened`/`BrowserClosed`, `TabsChanged`,
  `Navigated`. With these the UI can show that the agent is browsing and open
  the panel on its own.
- **Browser lifetime.**
  - A browser that is being viewed is not closed at the end of the turn; it
    closes after an idle period or when the panel releases it.
  - Sub-agent browsers either use the session's manager or register with it.
- **Who is in control.** User and agent acting on the same page break refs and
  confuse the model.
  - The agent is in control by default; the user can take over.
  - While the user is in control, browser tools return a clear error ("the
    user is controlling the browser") or wait.
  - After user actions, the next tool result carries a note ("the user
    navigated to X; read the page again"), the same way actions report
    navigations today.
- **Where the agent clicks.** The tool layer knows the coordinates of every
  `browser_computer` action. Sent as an event, the panel can draw a short
  pulse at that point over the frame, which makes the agent easy to follow.

### `ui_gpui` (layer 4)

- `RightPanelView::Browser`, a Review | Browser switcher in the panel header,
  persisted like the review view.
- `BrowserView` entity:
  - toolbar: tabs, URL field, back/forward/reload, profile picker, take-over
    button,
  - frame area.
- **Frames.** Decode JPEG off the main thread (`image` is a dependency
  already) into a `RenderImage`. Release the previous frame with
  `Window::drop_image`, or the sprite atlas grows.
- **Coordinates.** Panel coordinates map to page CSS pixels through the
  screencast frame metadata (`deviceWidth`, `offsetTop`, `pageScaleFactor`).
- **Size.** Show the 1280×800 viewport scaled down rather than resizing the
  viewport to the panel; resizing would move the agent's coordinate frame
  under it. Optional: a "fit to panel" mode, or a wider panel for this view.
- **Keyboard.** Focus handle plus `EntityInputHandler` for IME. Mapping GPUI
  `Keystroke` to CDP key events is a table to write, but self-contained.
- **Cursor shape.** CDP does not report it; leave it out at first.

### Login in the panel

With a live, interactive panel, `browser_login` often needs neither a second,
headful Chrome window nor the switch back to headless
(`tools/impls/browser/profiles.rs`): the user logs in inside the panel.
Limits:

- some sites (Google in particular) detect headless Chrome and block the
  login,
- client certificates (Elster) and passkeys need native dialogs.

So the headful fallback stays.

## Stage 2: a built-in browser

### CEF with off-screen rendering (preferred)

- Chromium runs in the app's own process tree, no installed Chrome needed.
- On macOS `OnAcceleratedPaint` hands over an IOSurface, which GPUI's
  `surface()` element draws without a copy through the CPU. Popups arrive as
  their own paint type (`PET_POPUP`), which solves `<select>`.
- **Tools unchanged.** CEF starts with `--remote-debugging-port`;
  chromiumoxide uses `Browser::connect(ws_url)`. `LaunchedBrowser` becomes an
  enum `Launched | Connected`. `Tab`, the AX tree and the tool set stay as
  they are.
- **Profiles** become `CefRequestContext`s with their own `cache_path` under a
  common root.
- **Costs and risks.**
  - The framework adds roughly 200–250 MB.
  - The macOS app bundle needs helper bundles (GPU, renderer, plugin), each
    signed (see `docs/macos-signing.md`).
  - CEF's message loop has to run inside GPUI's run loop
    (`external_message_pump`).
  - Linux and Windows are separate work each.
  - Rust bindings (`tauri-apps/cef-rs`) are the largest risk.
- **Gains.** Native popups and cursor, lower latency, no headless detection,
  no Chrome on the machine.

### Native web view (WKWebView via `wry`): not recommended

- WebKit does not speak CDP. Every browser tool would need a second backend,
  without a network log, a real accessibility tree or trusted input events.
- A native subview always sits above GPUI's drawing. Popovers, toasts,
  dropdown animations and the sidebar animation would be covered or clipped
  wrongly.

## Further ideas

- **One browser for user and agent**, as in Claude Code Desktop: the user
  browses in the panel and the agent can look at "this page". Needs a
  user-visible default profile per session or project that tools use when no
  `profile` is given.
- **Attach to the user's own Chrome** over `--remote-debugging-port` with
  `Browser::connect`: logged in as the user, which fits pal. A security
  question; explicit opt-in only.
- **"Show in panel" on browser tool cards**, jumping to the right tab.
- **Read-only first.** Showing frames without input already covers most of the
  value at low risk; input and take-over follow.

## Order of work

1. `web`: screencast pump with several consumers, tab events; move `record`
   onto it (unit-testable).
2. Core: `browser_view` handle, metadata events, lifetime tied to the viewer
   instead of the turn, sub-agent browsers in the session's manager.
3. GPUI: read-only browser view, panel switcher, click pulse.
4. Input, take-over, notes in tool results.
5. Login in the panel, `<select>` overlay.
6. Optional: CEF spike behind a feature flag.
