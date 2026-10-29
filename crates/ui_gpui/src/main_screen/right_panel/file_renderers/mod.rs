//! Other ways than the text editor to show a file in the Files view, such as
//! rendered Markdown, a drawn SVG, or a plugin's web page.
//!
//! A built-in renderer is an entry in [`RENDERERS`]: an id, a label for the
//! viewer's dropdown, which files it takes and how to create its view.
//! Enabled plugins add their file viewers (`web`, with the `browser-panel`
//! feature, whose screencast they use) after those; both are a [`Renderer`]. The view is an entity implementing [`RendererView`]. The
//! [`FileViewer`] keeps loading, editing and saving the file; a renderer gets
//! the file's current text and the draft's comments, reports comments made in
//! it, and may change the text ([`RendererEdit`]). The text editor is always
//! offered as [`TEXT`] and is not an entry here.
//!
//! A file shows with the first renderer that takes it, unless the user
//! picked another way (the text editor, too) for its kind of file
//! ([`chosen`]).
//!
//! Adding a renderer: a module with a view implementing [`RendererView`] and
//! one [`FileRenderer`] entry in [`RENDERERS`].
//!
//! [`FileViewer`]: super::file_viewer::FileViewer

mod markdown;
mod svg;
#[cfg(feature = "browser-panel")]
mod web;

use super::file_viewer::FileViewer;
use crate::comments::CommentChange;
use code_assistant_core::line_comments::LineComment;
use code_assistant_core::plugin_host::PluginFileViewer;
use gpui_kit::{
    AnyView, App, Context, Entity, EventEmitter, Render, SharedString, Subscription, Window,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
#[cfg(feature = "browser-panel")]
use std::sync::Arc;

/// The id of the text editor, which every file can be shown in.
pub const TEXT: &str = "text";

/// One way of showing files.
pub struct FileRenderer {
    /// Stable id, remembered per session.
    pub id: &'static str,
    /// The name in the viewer's dropdown.
    pub label: &'static str,
    /// Whether it can show the file at this `/`-separated path.
    pub accepts: fn(path: &str) -> bool,
    /// Create the view for the file at the absolute path `file` in the
    /// project at `root`.
    pub create: fn(
        file: PathBuf,
        root: PathBuf,
        window: &mut Window,
        cx: &mut App,
    ) -> Box<dyn RendererHandle>,
}

/// Every built-in renderer besides the text editor.
pub const RENDERERS: &[FileRenderer] = &[markdown::RENDERER, svg::RENDERER];

/// A way to show a file besides the text editor.
#[derive(Clone)]
pub enum Renderer {
    BuiltIn(&'static FileRenderer),
    /// A plugin's web page; its id is `<plugin>/<viewer>`.
    #[cfg(feature = "browser-panel")]
    Plugin(Arc<PluginFileViewer>),
}

impl Renderer {
    pub fn id(&self) -> SharedString {
        match self {
            Self::BuiltIn(r) => r.id.into(),
            #[cfg(feature = "browser-panel")]
            Self::Plugin(v) => v.key().into(),
        }
    }

    pub fn label(&self) -> SharedString {
        match self {
            Self::BuiltIn(r) => r.label.into(),
            #[cfg(feature = "browser-panel")]
            Self::Plugin(v) => v.spec.label.clone().into(),
        }
    }

    /// Create the view for the file at the absolute path `file` in the
    /// project at `root`.
    pub fn create(
        &self,
        file: PathBuf,
        root: PathBuf,
        window: &mut Window,
        cx: &mut App,
    ) -> Box<dyn RendererHandle> {
        match self {
            Self::BuiltIn(r) => (r.create)(file, root, window, cx),
            #[cfg(feature = "browser-panel")]
            Self::Plugin(v) => web::create(v.clone(), file, root, window, cx),
        }
    }
}

/// The renderers that can show `path`, besides the text editor: the
/// built-in ones, then the enabled plugins' viewers.
pub fn for_path(path: &str) -> Vec<Renderer> {
    for_path_with(
        path,
        &code_assistant_core::plugin_host::catalog().file_viewers(),
    )
}

fn for_path_with(path: &str, viewers: &[PluginFileViewer]) -> Vec<Renderer> {
    let built_in = RENDERERS
        .iter()
        .filter(|r| (r.accepts)(path))
        .map(Renderer::BuiltIn);
    #[cfg(feature = "browser-panel")]
    let plugins = viewers
        .iter()
        .filter(|v| v.spec.accepts(path))
        .map(|v| Renderer::Plugin(Arc::new(v.clone())));
    #[cfg(not(feature = "browser-panel"))]
    let plugins = viewers.iter().filter_map(|_| None);
    built_in.chain(plugins).collect()
}

/// What renderer picks are remembered by: the lowercase extension, or the
/// file name for a file without one.
pub fn file_kind(path: &str) -> String {
    let path = Path::new(path);
    path.extension()
        .or_else(|| path.file_name())
        .map(|k| k.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// The renderer `path` shows with: the one picked for its kind of file in
/// `picks`, else the first that takes it; `None` for the text editor. A pick
/// that does not take this file (a draw.io viewer picked for a `.drawio.svg`
/// file, asked about another `.svg`) leaves it to the first that does.
pub fn chosen(picks: &BTreeMap<String, String>, path: &str) -> Option<Renderer> {
    chosen_from(picks, path, for_path(path))
}

fn chosen_from(
    picks: &BTreeMap<String, String>,
    path: &str,
    renderers: Vec<Renderer>,
) -> Option<Renderer> {
    match picks.get(&file_kind(path)) {
        Some(id) if id == TEXT => None,
        Some(id) => renderers
            .iter()
            .find(|r| r.id() == id.as_str())
            .or(renderers.first())
            .cloned(),
        None => renderers.into_iter().next(),
    }
}

/// Whether `path` has one of `extensions` (lowercase, without the dot).
pub fn has_extension(path: &str, extensions: &[&str]) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| extensions.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// A renderer changed the file's text (a web viewer saved an edit): the
/// viewer puts it into the editor, which saves it as any edit.
#[derive(Debug, Clone)]
pub struct RendererEdit(pub String);

/// The view of a renderer. Everything but the text is optional; a view
/// that cannot show a line or a comment leaves the defaults, and the file
/// then opens in the text editor for it.
pub trait RendererView:
    Render + EventEmitter<CommentChange> + EventEmitter<RendererEdit> + Sized
{
    /// The file's text, at first and whenever it changes.
    fn set_text(&mut self, text: &str, cx: &mut Context<Self>);

    /// Every comment of the draft, to mark those on this file.
    fn set_comments(&mut self, _comments: &[LineComment], _cx: &mut Context<Self>) {}

    /// Show line `line` (1-based) of the file; `false` when the view
    /// cannot.
    fn reveal_line(&mut self, _line: usize, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        false
    }

    /// Show `comment` (on this file) and open it for editing; `false` when
    /// the view cannot.
    fn reveal_comment(
        &mut self,
        _comment: LineComment,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> bool {
        false
    }

    /// Comment on the current selection (Cmd/Ctrl-Shift-M).
    fn start_comment(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}
}

/// A renderer's view as the [`FileViewer`] holds it, whatever its type.
pub trait RendererHandle {
    fn view(&self) -> AnyView;
    fn set_text(&self, text: &str, cx: &mut App);
    fn set_comments(&self, comments: &[LineComment], cx: &mut App);
    fn reveal_line(&self, line: usize, window: &mut Window, cx: &mut App) -> bool;
    fn reveal_comment(&self, comment: LineComment, window: &mut Window, cx: &mut App) -> bool;
    fn start_comment(&self, window: &mut Window, cx: &mut App);
    /// Pass the view's comment changes on as the viewer's.
    fn forward_comments(&self, cx: &mut Context<FileViewer>) -> Subscription;
    /// Put the view's text changes into the viewer's editor.
    fn forward_edits(&self, window: &mut Window, cx: &mut Context<FileViewer>) -> Subscription;
}

impl<V: RendererView> RendererHandle for Entity<V> {
    fn view(&self) -> AnyView {
        self.clone().into()
    }

    fn set_text(&self, text: &str, cx: &mut App) {
        self.update(cx, |view, cx| view.set_text(text, cx));
    }

    fn set_comments(&self, comments: &[LineComment], cx: &mut App) {
        self.update(cx, |view, cx| view.set_comments(comments, cx));
    }

    fn reveal_line(&self, line: usize, window: &mut Window, cx: &mut App) -> bool {
        self.update(cx, |view, cx| view.reveal_line(line, window, cx))
    }

    fn reveal_comment(&self, comment: LineComment, window: &mut Window, cx: &mut App) -> bool {
        self.update(cx, |view, cx| view.reveal_comment(comment, window, cx))
    }

    fn start_comment(&self, window: &mut Window, cx: &mut App) {
        self.update(cx, |view, cx| view.start_comment(window, cx));
    }

    fn forward_comments(&self, cx: &mut Context<FileViewer>) -> Subscription {
        cx.subscribe(self, |_, _, change: &CommentChange, cx| {
            cx.emit(change.clone())
        })
    }

    fn forward_edits(&self, window: &mut Window, cx: &mut Context<FileViewer>) -> Subscription {
        cx.subscribe_in(
            self,
            window,
            |viewer, _, edit: &RendererEdit, window, cx| {
                viewer.apply_renderer_edit(&edit.0, window, cx)
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderers_are_offered_by_extension() {
        let ids = |path| {
            for_path_with(path, &[])
                .iter()
                .map(|r| r.id().to_string())
                .collect::<Vec<_>>()
        };
        let find = |id: &str, path| for_path_with(path, &[]).into_iter().find(|r| r.id() == id);
        assert_eq!(ids("docs/plan.md"), ["markdown"]);
        assert_eq!(ids("README.MARKDOWN"), ["markdown"]);
        assert_eq!(ids("art/logo.svg"), ["svg"]);
        assert_eq!(ids("docs/flow.drawio.svg"), ["svg"]);
        assert!(ids("src/lib.rs").is_empty());
        assert!(ids("Makefile").is_empty());
        assert!(find("markdown", "a.md").is_some());
        assert!(find("markdown", "a.rs").is_none());
    }

    #[cfg(feature = "browser-panel")]
    fn drawio_viewer() -> PluginFileViewer {
        use code_assistant_core::plugin_host::{FileViewerSpec, ViewerProtocol};
        PluginFileViewer {
            plugin: "diagrams".into(),
            root: PathBuf::from("/plugins/diagrams"),
            spec: FileViewerSpec {
                id: "drawio".into(),
                label: "draw.io".into(),
                files: vec!["*.drawio".into(), "*.drawio.svg".into()],
                url: "http://localhost:8080/".into(),
                protocol: ViewerProtocol::Drawio,
                start: None,
            },
        }
    }

    #[cfg(feature = "browser-panel")]
    #[test]
    fn plugin_viewers_follow_the_built_in_renderers() {
        let viewers = [drawio_viewer()];
        let ids = |path| {
            for_path_with(path, &viewers)
                .iter()
                .map(|r| r.id().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("docs/flow.drawio"), ["diagrams/drawio"]);
        assert_eq!(ids("docs/flow.drawio.svg"), ["svg", "diagrams/drawio"]);
        assert_eq!(ids("art/logo.svg"), ["svg"]);

        // draw.io picked for a .drawio.svg is remembered for .svg files; one
        // it does not take still shows as an image.
        let mut picks = BTreeMap::new();
        picks.insert(file_kind("flow.drawio.svg"), "diagrams/drawio".to_string());
        let id = |path| {
            chosen_from(&picks, path, for_path_with(path, &viewers)).map(|r| r.id().to_string())
        };
        assert_eq!(id("flow.drawio.svg").as_deref(), Some("diagrams/drawio"));
        assert_eq!(id("logo.svg").as_deref(), Some("svg"));
    }

    #[test]
    fn a_pick_holds_for_its_kind_of_file() {
        let id = |picks: &BTreeMap<String, String>, path| {
            chosen_from(picks, path, for_path_with(path, &[])).map(|r| r.id().to_string())
        };
        let mut picks = BTreeMap::new();
        assert_eq!(
            id(&picks, "a.md").as_deref(),
            Some("markdown"),
            "rendered by default"
        );
        assert_eq!(id(&picks, "a.rs").as_deref(), None);
        picks.insert(file_kind("docs/B.MD"), TEXT.to_owned());
        assert_eq!(id(&picks, "a.md").as_deref(), None, "text picked for .md");
        assert_eq!(id(&picks, "a.markdown").as_deref(), Some("markdown"));
        assert_eq!(file_kind("Makefile"), "makefile");
    }

    #[test]
    fn renderer_ids_are_unique_and_not_text() {
        let mut ids: Vec<_> = RENDERERS.iter().map(|r| r.id).collect();
        assert!(!ids.contains(&TEXT));
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), RENDERERS.len());
    }
}
