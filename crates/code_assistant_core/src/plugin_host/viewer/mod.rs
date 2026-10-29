//! File viewers from plugins: a web page that shows a file, and may edit it.
//!
//! The page runs in a headless Chrome tab of its own, shown live through the
//! screencast the browser panel uses ([`web::LiveFrames`]), with the user's
//! mouse and keyboard forwarded. A small script in the page ([`protocol`])
//! carries the file's text in and edits out: the frontend's file viewer
//! keeps loading and saving the file, as for the text editor.
//!
//! [`open`] starts the viewer's server when needed ([`server`]), opens the
//! tab and returns its frames, a [`ViewerControl`] and the page's
//! [`ViewerEvent`]s. Dropping every `ViewerControl` closes the browser.

pub mod server;

use super::catalog::{PluginFileViewer, catalog};
use super::manifest::{FileViewerSpec, ViewerProtocol};
use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use web::{BrowserLaunchConfig, BrowserSession, LiveFrames, Tab, UserInput};

/// The function the page's script calls with JSON messages.
const HOST_BINDING: &str = "__codeAssistantHost";
/// Smallest viewport the page gets, in CSS pixels.
const MIN_VIEWPORT: (u32, u32) = (320, 240);

/// What to open.
#[derive(Debug, Clone)]
pub struct ViewerRequest {
    /// The viewer, `<plugin>/<viewer>`.
    pub viewer: String,
    /// The file shown, absolute.
    pub file: PathBuf,
    /// The project the file is in.
    pub root: PathBuf,
    /// The file's current text.
    pub text: String,
    /// The page's size in CSS pixels.
    pub viewport: (u32, u32),
    /// The largest frame wanted, in pixels.
    pub max_frame: (u32, u32),
}

/// An open viewer.
pub struct WebViewer {
    pub frames: LiveFrames,
    pub control: ViewerControl,
    pub events: mpsc::UnboundedReceiver<ViewerEvent>,
}

/// What the page reports.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewerEvent {
    /// The page changed the file's text: write it.
    TextChanged(String),
    /// The page could not be loaded.
    Failed(String),
}

enum Command {
    Input(UserInput),
    SetText(String),
    Resize(u32, u32),
    SelectedText(oneshot::Sender<Result<String>>),
}

/// Acts on an open viewer. Clones share one queue; the browser closes when
/// the last clone is dropped.
#[derive(Clone)]
pub struct ViewerControl {
    queue: mpsc::UnboundedSender<Command>,
}

impl ViewerControl {
    /// Forward the user's mouse or keyboard.
    pub fn input(&self, input: UserInput) {
        let _ = self.queue.send(Command::Input(input));
    }

    /// The file's text changed (on disk or in the editor).
    pub fn set_text(&self, text: String) {
        let _ = self.queue.send(Command::SetText(text));
    }

    /// The view's size changed: lay the page out for `width`×`height` CSS px.
    pub fn resize(&self, width: u32, height: u32) {
        let _ = self.queue.send(Command::Resize(width, height));
    }

    /// The text selected on the page.
    pub async fn selected_text(&self) -> Result<String> {
        let (tx, rx) = oneshot::channel();
        self.queue
            .send(Command::SelectedText(tx))
            .map_err(|_| anyhow!("the viewer closed"))?;
        rx.await.map_err(|_| anyhow!("the viewer closed"))?
    }
}

/// Open a viewer for `request`. Needs a tokio runtime. Returns once the tab
/// is ready; the page loads afterwards (a failure arrives as
/// [`ViewerEvent::Failed`]).
pub async fn open(request: ViewerRequest) -> Result<WebViewer> {
    let viewer = catalog()
        .file_viewer(&request.viewer)
        .ok_or_else(|| anyhow!("no enabled plugin offers the viewer {}", request.viewer))?;
    let server = server::ensure(&viewer).await?;
    let url = page_url(&viewer.spec, &request.file, &request.root);
    let session = Arc::new(
        BrowserSession::open(
            BrowserLaunchConfig::default(),
            format!("viewer {}", viewer.key()),
        )
        .await?,
    );
    let tab = session.active_tab()?;
    let (width, height) = viewport(request.viewport);
    tab.set_viewport(width, height, false).await?;
    let binding = match viewer.spec.protocol {
        ViewerProtocol::None => None,
        _ => Some(tab.bind(HOST_BINDING).await?),
    };
    if let Some(script) = page_script(viewer.spec.protocol, &request.file, &request.root) {
        tab.add_init_script(&script).await?;
    }
    let frames = tab.live_frames(request.max_frame);
    let (queue, commands) = mpsc::unbounded_channel();
    let (events_tx, events) = mpsc::unbounded_channel();
    tokio::spawn(run(Running {
        viewer,
        session,
        tab,
        url,
        binding,
        commands,
        events: events_tx,
        text: request.text,
        server,
    }));
    Ok(WebViewer {
        frames,
        control: ViewerControl { queue },
        events,
    })
}

struct Running {
    viewer: PluginFileViewer,
    session: Arc<BrowserSession>,
    tab: Arc<Tab>,
    url: String,
    binding: Option<web::Binding>,
    commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::UnboundedSender<ViewerEvent>,
    text: String,
    server: Option<Arc<server::StartedServer>>,
}

/// Load the page, then pass text and input between it and the frontend
/// until every [`ViewerControl`] is gone.
async fn run(mut r: Running) {
    tracing::debug!("Viewer {} opens {}", r.viewer.key(), r.url);
    if let Err(e) = r.tab.navigate(&r.url).await {
        let _ = r.events.send(ViewerEvent::Failed(format!(
            "Could not load {}: {e:#}",
            r.url
        )));
    }
    // The text the page holds, to pass on only what it does not have.
    let mut page_text: Option<String> = None;
    let mut ready = false;
    loop {
        let call = async {
            match &mut r.binding {
                Some(binding) => binding.next().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            command = r.commands.recv() => match command {
                None => break,
                Some(Command::Input(input)) => {
                    if let Err(e) = r.tab.user_input(input).await {
                        tracing::debug!("viewer input failed: {e:#}");
                    }
                }
                Some(Command::SetText(text)) => {
                    r.text = text;
                    if ready && page_text.as_ref() != Some(&r.text) {
                        deliver(&r.tab, &r.text, &mut page_text).await;
                    }
                }
                Some(Command::Resize(width, height)) => {
                    let (width, height) = viewport((width, height));
                    if let Err(e) = r.tab.set_viewport(width, height, false).await {
                        tracing::debug!("viewer resize failed: {e:#}");
                    }
                }
                Some(Command::SelectedText(reply)) => {
                    let _ = reply.send(r.tab.javascript(SELECTED_TEXT_JS).await);
                }
            },
            Some(payload) = call => match serde_json::from_str::<PageMessage>(&payload) {
                // A (new) document's script is listening: give it the text.
                Ok(PageMessage::Ready) => {
                    ready = true;
                    page_text = None;
                    deliver(&r.tab, &r.text, &mut page_text).await;
                }
                Ok(PageMessage::Text { text }) => {
                    if page_text.as_ref() != Some(&text) {
                        page_text = Some(text.clone());
                        r.text = text.clone();
                        let _ = r.events.send(ViewerEvent::TextChanged(text));
                    }
                }
                Err(e) => tracing::debug!("viewer: unreadable page message: {e}"),
            },
        }
    }
    tracing::debug!("Viewer {} closes", r.viewer.key());
    r.session.close().await;
    if let Some(server) = r.server {
        tokio::spawn(async move {
            tokio::time::sleep(server::LINGER).await;
            drop(server);
        });
    }
}

/// A message from the page's script.
#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum PageMessage {
    Ready,
    Text { text: String },
}

async fn deliver(tab: &Tab, text: &str, page_text: &mut Option<String>) {
    let script = format!(
        "window.__codeAssistant && window.__codeAssistant.receiveText({})",
        serde_json::Value::String(text.to_string())
    );
    match tab.javascript(&script).await {
        Ok(_) => *page_text = Some(text.to_string()),
        Err(e) => tracing::debug!("viewer: could not pass the text on: {e:#}"),
    }
}

fn viewport((width, height): (u32, u32)) -> (u32, u32) {
    (width.max(MIN_VIEWPORT.0), height.max(MIN_VIEWPORT.1))
}

/// The selection inside a focused text field, else the document's.
const SELECTED_TEXT_JS: &str = "(() => { const el = document.activeElement; \
    if (el && typeof el.selectionStart === 'number' && el.value !== undefined) \
      return el.value.slice(el.selectionStart, el.selectionEnd); \
    return String(getSelection()); })()";

/// The script that connects the page to the app, by protocol.
fn page_script(protocol: ViewerProtocol, file: &Path, root: &Path) -> Option<String> {
    let source = match protocol {
        ViewerProtocol::HostApi => include_str!("host_api.js"),
        ViewerProtocol::Drawio => include_str!("drawio.js"),
        ViewerProtocol::None => return None,
    };
    let file_info = serde_json::json!({
        "path": file.display().to_string(),
        "relativePath": relative(file, root),
    });
    Some(source.replace("__FILE__", &file_info.to_string()))
}

fn relative(file: &Path, root: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

/// The page for `file`: the viewer's URL with `{path}` and `{relative_path}`
/// filled in, and for draw.io the embed parameters it does not set itself.
pub fn page_url(spec: &FileViewerSpec, file: &Path, root: &Path) -> String {
    let mut url = spec
        .url
        .replace("{path}", &urlencoding::encode(&file.display().to_string()))
        .replace(
            "{relative_path}",
            &urlencoding::encode(&relative(file, root)),
        );
    if spec.protocol == ViewerProtocol::Drawio {
        let (base, fragment) = match url.split_once('#') {
            Some((base, fragment)) => (base.to_string(), Some(fragment.to_string())),
            None => (url.clone(), None),
        };
        let query = base.split_once('?').map(|(_, q)| q).unwrap_or_default();
        let has = |key: &str| {
            query
                .split('&')
                .any(|pair| pair.split('=').next() == Some(key))
        };
        let missing: Vec<String> = DRAWIO_PARAMS
            .iter()
            .filter(|(key, _)| !has(key))
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        url = base;
        if !missing.is_empty() {
            url.push(if url.contains('?') { '&' } else { '?' });
            url.push_str(&missing.join("&"));
        }
        if let Some(fragment) = fragment {
            url.push('#');
            url.push_str(&fragment);
        }
    }
    url
}

/// draw.io's embed mode with autosave and without its save/exit buttons.
const DRAWIO_PARAMS: &[(&str, &str)] = &[
    ("embed", "1"),
    ("proto", "json"),
    ("spin", "1"),
    ("saveAndExit", "0"),
    ("noSaveBtn", "1"),
    ("noExitBtn", "1"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(url: &str, protocol: ViewerProtocol) -> FileViewerSpec {
        FileViewerSpec {
            id: "v".into(),
            label: "V".into(),
            files: vec!["*".into()],
            url: url.into(),
            protocol,
            start: None,
        }
    }

    #[test]
    fn urls_get_the_file_and_draw_io_its_embed_parameters() {
        let file = Path::new("/work/proj/docs/a b.drawio");
        let root = Path::new("/work/proj");
        assert_eq!(
            page_url(
                &spec("http://h/view?f={relative_path}", ViewerProtocol::None),
                file,
                root
            ),
            "http://h/view?f=docs%2Fa%20b.drawio"
        );
        assert_eq!(
            page_url(
                &spec("http://h/?p={path}", ViewerProtocol::HostApi),
                file,
                root
            ),
            "http://h/?p=%2Fwork%2Fproj%2Fdocs%2Fa%20b.drawio"
        );
        assert_eq!(
            page_url(
                &spec("http://localhost:8080/", ViewerProtocol::Drawio),
                file,
                root
            ),
            "http://localhost:8080/?embed=1&proto=json&spin=1&saveAndExit=0&noSaveBtn=1&noExitBtn=1"
        );
        assert_eq!(
            page_url(
                &spec("http://h/?ui=dark&spin=0#x", ViewerProtocol::Drawio),
                file,
                root
            ),
            "http://h/?ui=dark&spin=0&embed=1&proto=json&saveAndExit=0&noSaveBtn=1&noExitBtn=1#x"
        );
    }

    /// A page that uses the host API, and one that speaks draw.io's embed
    /// protocol the way draw.io does: text reaches each, and its edits come
    /// back. Needs Chrome, like the browser tool tests.
    #[tokio::test(flavor = "multi_thread")]
    async fn viewer_pages_exchange_the_text() {
        use axum::{Router, response::Html, routing::get};
        const HOST_PAGE: &str = "<script>codeAssistant.onText(t => { \
            if (!t.endsWith('!')) codeAssistant.setText(t + '!'); });</script>";
        const DRAWIO_PAGE: &str = "<script>\
            window.addEventListener('message', e => { \
              if (e.source !== window.parent) return; \
              const m = JSON.parse(e.data); \
              if (m.action === 'load') window.parent.postMessage( \
                JSON.stringify({ event: 'autosave', xml: m.xml + '<edited/>' }), '*'); \
            }); \
            window.parent.postMessage(JSON.stringify({ event: 'init' }), '*');</script>";
        let app = Router::new()
            .route("/host", get(|| async { Html(HOST_PAGE) }))
            .route("/drawio", get(|| async { Html(DRAWIO_PAGE) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let config = tempfile::tempdir().unwrap();
        let plugin = tempfile::tempdir().unwrap();
        let manifest = plugin.path().join(crate::plugin_host::MANIFEST_PATH);
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(
            &manifest,
            format!(
                r#"{{ "name": "t", "fileViewers": [
                    {{ "id": "host", "label": "Host", "files": ["*.txt"],
                       "url": "http://{addr}/host" }},
                    {{ "id": "drawio", "label": "draw.io", "files": ["*.drawio"],
                       "url": "http://{addr}/drawio", "protocol": "drawio" }}
                ] }}"#
            ),
        )
        .unwrap();
        let root = plugin.path().to_path_buf();
        async fn next_text(viewer: &mut WebViewer) -> String {
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(20), viewer.events.recv())
                    .await
                    .expect("the page answers");
            match event {
                Some(ViewerEvent::TextChanged(text)) => text,
                other => panic!("unexpected {other:?}"),
            }
        }
        temp_env::async_with_vars(
            [("CODE_ASSISTANT_CONFIG_DIR", Some(config.path()))],
            async {
                crate::plugin_host::PluginStore::new()
                    .install(root.to_str().unwrap())
                    .unwrap();
                let request = |viewer: &str, file: &str, text: &str| ViewerRequest {
                    viewer: viewer.into(),
                    file: root.join(file),
                    root: root.clone(),
                    text: text.into(),
                    viewport: (800, 600),
                    max_frame: (800, 600),
                };

                let mut host = open(request("t/host", "a.txt", "hello")).await.unwrap();
                assert_eq!(next_text(&mut host).await, "hello!");

                let mut drawio = open(request("t/drawio", "a.drawio", "<mxfile/>"))
                    .await
                    .unwrap();
                assert_eq!(next_text(&mut drawio).await, "<mxfile/><edited/>");
                drawio.control.set_text("<b/>".into());
                assert_eq!(next_text(&mut drawio).await, "<b/><edited/>");
            },
        )
        .await;
    }

    /// Against the real draw.io (`DRAWIO_URL`, e.g.
    /// `https://embed.diagrams.net/` or a local `jgraph/drawio`): a diagram
    /// loads, and deleting its shapes with the keyboard writes the change
    /// back. Opt-in: needs Chrome and the network.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn real_drawio_loads_and_writes_back() {
        let url = std::env::var("DRAWIO_URL").expect("set DRAWIO_URL");
        const DIAGRAM: &str = r#"<mxfile><diagram id="d" name="Page-1"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/><mxCell id="hello" value="Hello" style="rounded=1;" vertex="1" parent="1"><mxGeometry x="200" y="150" width="120" height="60" as="geometry"/></mxCell></root></mxGraphModel></diagram></mxfile>"#;
        let config = tempfile::tempdir().unwrap();
        let plugin = tempfile::tempdir().unwrap();
        let manifest = plugin.path().join(crate::plugin_host::MANIFEST_PATH);
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(
            &manifest,
            serde_json::json!({ "name": "d", "fileViewers": [{
                "id": "drawio", "label": "draw.io", "files": ["*.drawio"],
                "url": url, "protocol": "drawio" }] })
            .to_string(),
        )
        .unwrap();
        let root = plugin.path().to_path_buf();
        temp_env::async_with_vars(
            [("CODE_ASSISTANT_CONFIG_DIR", Some(config.path()))],
            async {
                crate::plugin_host::PluginStore::new()
                    .install(root.to_str().unwrap())
                    .unwrap();
                let mut viewer = open(ViewerRequest {
                    viewer: "d/drawio".into(),
                    file: root.join("a.drawio"),
                    root: root.clone(),
                    text: DIAGRAM.into(),
                    viewport: (1200, 800),
                    max_frame: (1200, 800),
                })
                .await
                .unwrap();
                // draw.io loads its scripts, then the diagram.
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                let key = |key: &str, modifiers| UserInput::KeyDown {
                    key: key.into(),
                    text: None,
                    modifiers,
                    commands: vec![],
                };
                let ctrl = if cfg!(target_os = "macos") { 4 } else { 2 };
                viewer.control.input(key("a", ctrl));
                viewer.control.input(UserInput::KeyUp {
                    key: "a".into(),
                    modifiers: ctrl,
                });
                viewer.control.input(key("Delete", 0));
                viewer.control.input(UserInput::KeyUp {
                    key: "Delete".into(),
                    modifiers: 0,
                });
                let changed = tokio::time::timeout(std::time::Duration::from_secs(20), async {
                    loop {
                        match viewer.events.recv().await {
                            Some(ViewerEvent::TextChanged(text)) => return text,
                            Some(ViewerEvent::Failed(e)) => panic!("{e}"),
                            None => panic!("the viewer closed"),
                        }
                    }
                })
                .await
                .expect("draw.io saves the edit");
                assert!(changed.contains("<mxGraphModel"), "{changed}");
                assert!(!changed.contains("Hello"), "the shape is gone: {changed}");
            },
        )
        .await;
    }

    #[test]
    fn page_scripts_know_the_file() {
        let script = page_script(
            ViewerProtocol::HostApi,
            Path::new("/p/docs/x.md"),
            Path::new("/p"),
        )
        .unwrap();
        assert!(script.contains(r#""relativePath":"docs/x.md""#));
        assert!(!script.contains("__FILE__"));
        assert!(page_script(ViewerProtocol::None, Path::new("/p/x"), Path::new("/p")).is_none());
    }
}
