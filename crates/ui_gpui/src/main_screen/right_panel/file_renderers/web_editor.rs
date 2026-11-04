//! Files edited in place by a web app in a webview over the view (macOS
//! only for now). A renderer of this kind is set up in the settings
//! ([`WebEditor`](crate::shared::custom_renderers::WebEditor)): the
//! editor's address, and scripts to inject into it.
//!
//! The webview is a native view (`wry`, WebKit) laid over the window where
//! this view is painted, and hidden in frames that do not paint it
//! ([`native_views`]). It loads a small page holding the editor in an
//! iframe; the two talk JSON over `postMessage` (the *embed protocol*):
//!
//! - the editor sends `{"event": "init"}` once it has started;
//! - the page answers `{"action": "load", "xml": <text>, "autosave": 1,
//!   "title": <file name>}`;
//! - the editor sends every change as `{"event": "autosave", "xml": …}`
//!   (and a save as `"save"`), which goes to the viewer as a [`TextEdit`]
//!   and is saved like typing in the text editor.
//!
//! The page relays messages between the editor and Rust
//! (`window.ipc.postMessage`, `evaluate_script`).
//!
//! Scripts injected into the editor's frame say when it takes the keyboard
//! (GPUI's focus is cleared then, or its key bindings would take keys such
//! as Delete), and run the configured scripts once the frame has loaded.
//! A script that fails because what it uses is not there yet (a
//! `ReferenceError` or `TypeError`) is tried again for a while, so it can
//! wait for the editor's own scripts to set up the hooks it needs.
//!
//! Text from outside (a reload after the file changed on disk) is loaded
//! into the editor again; the editor's own edits coming back are not.
//!
//! [`native_views`]: crate::shared::native_views

use super::{RendererHandle, RendererView, TextEdit};
use crate::comments::CommentChange;
use crate::shared::custom_renderers::{self, WebEditor};
use crate::shared::native_views::{self, NativeView, Slot};
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    App, Bounds, Context, EventEmitter, Pixels, Render, SharedString, Task, Window, canvas, div,
    prelude::*,
};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use tracing::{debug, warn};
use wry::dpi::{LogicalPosition, LogicalSize};

pub fn create(
    label: SharedString,
    config: WebEditor,
    file: PathBuf,
    cx: &mut App,
) -> Box<dyn RendererHandle> {
    Box::new(cx.new(|cx| WebEditorView::new(label, config, file, cx)))
}

/// The configured address with `{dark}` filled in.
fn editor_url(url: &str, dark: bool) -> String {
    url.trim().replace("{dark}", if dark { "1" } else { "0" })
}

/// `scheme://host[:port]` of `url`.
fn origin(url: &str) -> &str {
    let start = url.find("://").map_or(0, |i| i + 3);
    url[start..]
        .find(['/', '?', '#'])
        .map_or(url, |end| &url[..start + end])
}

/// The page around the editor: relays its messages to Rust and offers
/// `editorSend` for messages to it.
fn page(url: &str, background: &str) -> String {
    let url = url.replace('"', "&quot;");
    format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8">
<style>
  html, body, iframe {{ margin: 0; padding: 0; border: 0; width: 100%; height: 100%;
    overflow: hidden; display: block; background: {background}; }}
</style></head>
<body><iframe id="editor" src="{url}"></iframe>
<script>
  const frame = document.getElementById('editor');
  window.addEventListener('message', (event) => {{
    if (event.source !== frame.contentWindow) return;
    const data = typeof event.data === 'string' ? event.data : JSON.stringify(event.data);
    window.ipc.postMessage(data);
  }});
  window.editorSend = (message) => frame.contentWindow.postMessage(JSON.stringify(message), '*');
</script></body></html>"#
    )
}

/// Runs in the editor's frame: says when the editor takes the keyboard (a
/// click into it).
const FOCUS_SCRIPT: &str = r#"
(() => {
  if (window === window.top) return;
  const post = () => window.parent.postMessage(JSON.stringify({ event: 'ca:focus' }), '*');
  window.addEventListener('pointerdown', post, true);
  window.addEventListener('focus', post, true);
})();
"#;

/// How long a script that fails for something missing is tried again, in
/// ms.
const SCRIPT_RETRY_MS: u32 = 30_000;

/// A script running `source` (named `name`) in the editor's frame at
/// `origin` once the frame has loaded, tried again while it fails for
/// something missing; it reports whether it ran (`ca:script`).
fn injected_script(name: &str, source: &str, origin: &str) -> String {
    let name = serde_json::to_string(name).unwrap_or_default();
    let origin = serde_json::to_string(origin).unwrap_or_default();
    format!(
        r#"
(() => {{
  if (window === window.top || window.location.origin !== {origin}) return;
  const report = (ok, error) => window.parent.postMessage(
    JSON.stringify({{ event: 'ca:script', name: {name}, ok, error }}), '*');
  const started = Date.now();
  const run = () => {{
    try {{
{source}
      report(true);
    }} catch (e) {{
      const missing = e instanceof ReferenceError || e instanceof TypeError;
      if (missing && Date.now() - started < {SCRIPT_RETRY_MS}) return setTimeout(run, 50);
      report(false, String(e));
    }}
  }};
  if (document.readyState === 'complete') run();
  else window.addEventListener('load', run, {{ once: true }});
}})();
"#
    )
}

struct Shown(Rc<wry::WebView>);

impl NativeView for Shown {
    fn set_shown(&self, shown: bool) {
        self.0.set_visible(shown).ok();
    }
}

/// The webview and its place among the window's native views.
struct Web {
    view: Rc<wry::WebView>,
    slot: Rc<Slot>,
}

pub struct WebEditorView {
    label: SharedString,
    config: WebEditor,
    file: PathBuf,
    /// The file's text: as handed in, or as the editor last saved it.
    text: String,
    /// The editor has started and takes messages.
    ready: bool,
    /// The editor took the keyboard: GPUI's focus is to be cleared, or its
    /// key bindings would take keys meant for the editor.
    blur: bool,
    /// Created when first painted (it needs the window).
    web: Rc<RefCell<Option<Web>>>,
    /// Why the editor cannot be shown.
    error: Rc<RefCell<Option<String>>>,
    messages: async_channel::Sender<String>,
    _receive: Task<()>,
}

impl EventEmitter<CommentChange> for WebEditorView {}
impl EventEmitter<TextEdit> for WebEditorView {}

impl WebEditorView {
    fn new(label: SharedString, config: WebEditor, file: PathBuf, cx: &mut Context<Self>) -> Self {
        let (messages, received) = async_channel::unbounded::<String>();
        let receive = cx.spawn(async move |this, cx| {
            while let Ok(message) = received.recv().await {
                if this
                    .update(cx, |this, cx| this.on_message(&message, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            label,
            config,
            file,
            text: String::new(),
            ready: false,
            blur: false,
            web: Rc::default(),
            error: Rc::default(),
            messages,
            _receive: receive,
        }
    }

    fn on_message(&mut self, message: &str, cx: &mut Context<Self>) {
        let Ok(message) = serde_json::from_str::<Value>(message) else {
            return;
        };
        let label = &self.label;
        match message["event"].as_str() {
            Some("init") => {
                self.ready = true;
                self.load();
            }
            Some("autosave" | "save") => {
                let Some(xml) = message["xml"].as_str() else {
                    return;
                };
                if xml != self.text {
                    self.text = xml.to_owned();
                    cx.emit(TextEdit(self.text.clone()));
                }
            }
            Some("ca:focus") => {
                self.blur = true;
                cx.notify();
            }
            Some("ca:script") => {
                let name = &message["name"];
                if message["ok"].as_bool() == Some(true) {
                    debug!("{label}: script {name} ran");
                } else {
                    warn!("{label}: script {name} failed: {}", message["error"]);
                }
            }
            event => debug!("{label}: {event:?}"),
        }
    }

    /// Show the file's text in the editor.
    fn load(&self) {
        let title = self
            .file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.send(json!({
            "action": "load",
            "xml": self.text,
            "autosave": 1,
            "title": title,
        }));
    }

    fn send(&self, message: Value) {
        if let Some(web) = self.web.borrow().as_ref()
            && let Err(e) = web
                .view
                .evaluate_script(&format!("window.editorSend({message})"))
        {
            warn!("{}: could not send a message: {e}", self.label);
        }
    }
}

/// Create the webview over `bounds` of `window`.
fn create_webview(
    config: &WebEditor,
    bounds: Bounds<Pixels>,
    dark: bool,
    messages: async_channel::Sender<String>,
    window: &mut Window,
    cx: &mut App,
) -> Result<Web, String> {
    let url = editor_url(&config.url, dark);
    let background = if dark { "#1e1e1e" } else { "#ffffff" };
    let mut builder = wry::WebViewBuilder::new()
        .with_html(page(&url, background))
        .with_bounds(rect(bounds))
        .with_initialization_script_for_main_only(FOCUS_SCRIPT, false)
        .with_ipc_handler(move |request| {
            messages.try_send(request.into_body()).ok();
        });
    for entry in &config.scripts {
        let path = custom_renderers::script_path(entry);
        let source = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read the script {}: {e}", path.display()))?;
        builder = builder.with_initialization_script_for_main_only(
            injected_script(entry, &source, origin(&url)),
            false,
        );
    }
    let view = builder.build_as_child(window).map_err(|e| e.to_string())?;
    let view = Rc::new(view);
    let slot = native_views::register(Box::new(Shown(view.clone())), window, cx);
    Ok(Web { view, slot })
}

fn rect(bounds: Bounds<Pixels>) -> wry::Rect {
    wry::Rect {
        position: LogicalPosition::new(
            f32::from(bounds.origin.x) as f64,
            f32::from(bounds.origin.y) as f64,
        )
        .into(),
        size: LogicalSize::new(
            f32::from(bounds.size.width) as f64,
            f32::from(bounds.size.height) as f64,
        )
        .into(),
    }
}

impl RendererView for WebEditorView {
    fn set_text(&mut self, text: &str, _cx: &mut Context<Self>) {
        // Also the editor's own edits, back from the text editor.
        if text == self.text {
            return;
        }
        self.text = text.to_owned();
        if self.ready {
            self.load();
        }
    }
}

impl Render for WebEditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.blur) {
            window.blur(cx);
        }
        let theme = cx.theme();
        if let Some(error) = self.error.borrow().clone() {
            return div()
                .size_full()
                .p_4()
                .bg(theme.background)
                .text_sm()
                .text_color(theme.danger)
                .child(SharedString::from(format!(
                    "{} could not open: {error}",
                    self.label
                )))
                .into_any_element();
        }
        let dark = theme.mode.is_dark();
        let config = self.config.clone();
        let web = self.web.clone();
        let error = self.error.clone();
        let messages = self.messages.clone();
        let entity = cx.entity().downgrade();
        div()
            .size_full()
            .bg(theme.background)
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, cx| {
                        let mut web = web.borrow_mut();
                        if web.is_none() {
                            match create_webview(
                                &config,
                                bounds,
                                dark,
                                messages.clone(),
                                window,
                                cx,
                            ) {
                                Ok(created) => *web = Some(created),
                                Err(e) => {
                                    *error.borrow_mut() = Some(e);
                                    entity.update(cx, |_, cx| cx.notify()).ok();
                                    return;
                                }
                            }
                        }
                        if let Some(web) = web.as_ref() {
                            web.view.set_bounds(rect(bounds)).ok();
                            web.slot.painted(cx);
                        }
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_fills_in_the_theme_and_names_its_origin() {
        assert_eq!(
            editor_url(" https://editor.example/?embed=1&dark={dark} ", true),
            "https://editor.example/?embed=1&dark=1"
        );
        assert_eq!(
            editor_url("https://editor.example/?dark={dark}", false),
            "https://editor.example/?dark=0"
        );
        assert_eq!(
            origin("https://editor.example/?embed=1"),
            "https://editor.example"
        );
        assert_eq!(
            origin("https://editor.example?embed=1"),
            "https://editor.example"
        );
        assert_eq!(
            origin("http://localhost:8080/editor/"),
            "http://localhost:8080"
        );
        assert_eq!(origin("https://editor.example"), "https://editor.example");
    }

    #[test]
    fn a_script_runs_in_the_editor_s_frame_only() {
        let script = injected_script("~/shapes.js", "Hooks.add(function() {});", "https://x");
        assert!(script.contains(r#"window.location.origin !== "https://x""#));
        assert!(script.contains("Hooks.add(function() {});"));
        assert!(script.contains(r#"name: "~/shapes.js""#));
    }
}
