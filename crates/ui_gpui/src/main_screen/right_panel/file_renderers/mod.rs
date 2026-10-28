//! Other ways than the text editor to show a file in the Files view, such as
//! rendered Markdown or a drawn SVG.
//!
//! A renderer is a [`FileRenderer`]: an id, a label for the viewer's
//! dropdown, which files it takes and how to create its view. The view is an
//! entity implementing [`RendererView`]. The [`FileViewer`] keeps loading,
//! editing and saving the file; a renderer gets the file's current text and
//! the draft's comments and reports comments made in it. The text editor is
//! always offered as [`TEXT`] and is not a renderer here.
//!
//! Renderers are compiled in ([`BUILT_IN`]) or defined by the user in the
//! settings ([`custom_renderers`]): a command that turns the file into an
//! SVG or an image (shown by [`command`]), or a web app editing it in place
//! (`web_editor`, macOS only). The user's come first.
//!
//! A file shows with the first renderer that takes it, unless the user
//! picked another way (the text editor, too) for its kind of file
//! ([`chosen`]).
//!
//! Adding a built-in renderer: a module with a view implementing
//! [`RendererView`] and one [`BuiltIn`] entry in [`BUILT_IN`].
//!
//! [`FileViewer`]: super::file_viewer::FileViewer
//! [`custom_renderers`]: crate::shared::custom_renderers

mod command;
mod markdown;
mod svg;
#[cfg(target_os = "macos")]
mod web_editor;

use super::file_viewer::FileViewer;
use crate::comments::CommentChange;
use crate::shared::custom_renderers::{self, CustomRenderer, RendererKind};
use code_assistant_core::line_comments::LineComment;
use gpui_kit::{
    AnyView, App, Context, Entity, EventEmitter, Render, SharedString, Subscription, Window,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The id of the text editor, which every file can be shown in.
pub const TEXT: &str = "text";

/// A renderer compiled into the app.
pub struct BuiltIn {
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

/// Every built-in renderer.
pub const BUILT_IN: &[BuiltIn] = &[markdown::RENDERER, svg::RENDERER];

/// One way of showing files.
#[derive(Clone)]
pub enum FileRenderer {
    BuiltIn(&'static BuiltIn),
    /// A command from the settings.
    Custom(Arc<CustomRenderer>),
}

impl PartialEq for FileRenderer {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::BuiltIn(a), Self::BuiltIn(b)) => a.id == b.id,
            // A changed command is another renderer: its view is made anew.
            (Self::Custom(a), Self::Custom(b)) => a == b,
            _ => false,
        }
    }
}

impl FileRenderer {
    /// Stable id, remembered per session.
    pub fn id(&self) -> String {
        match self {
            Self::BuiltIn(r) => r.id.to_owned(),
            Self::Custom(r) => r.id(),
        }
    }

    /// The name in the viewer's dropdown.
    pub fn label(&self) -> SharedString {
        match self {
            Self::BuiltIn(r) => r.label.into(),
            Self::Custom(r) => r.label.clone().into(),
        }
    }

    /// Whether it can show the file at this `/`-separated path.
    pub fn accepts(&self, path: &str) -> bool {
        match self {
            Self::BuiltIn(r) => (r.accepts)(path),
            Self::Custom(r) => r.accepts(path),
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
            Self::Custom(r) => match &r.kind {
                RendererKind::Command(command) => command::create(
                    r.label.clone().into(),
                    command.clone(),
                    file,
                    root,
                    window,
                    cx,
                ),
                #[cfg(target_os = "macos")]
                RendererKind::Web(editor) => {
                    web_editor::create(r.label.clone().into(), editor.clone(), file, cx)
                }
                // Not offered elsewhere (`all`).
                #[cfg(not(target_os = "macos"))]
                RendererKind::Web(_) => unreachable!("web editors are macOS only"),
            },
        }
    }
}

/// Every renderer besides the text editor: the user's this platform can
/// show, then the built-in ones.
pub fn all(cx: &App) -> Vec<FileRenderer> {
    custom_renderers::current(cx)
        .iter()
        .filter(|r| r.is_supported())
        .map(|r| FileRenderer::Custom(Arc::new(r.clone())))
        .chain(BUILT_IN.iter().map(FileRenderer::BuiltIn))
        .collect()
}

/// The renderers of `renderers` that can show `path`.
pub fn for_path<'a>(
    renderers: &'a [FileRenderer],
    path: &'a str,
) -> impl Iterator<Item = &'a FileRenderer> + 'a {
    renderers.iter().filter(move |r| r.accepts(path))
}

/// The renderer with `id`, when it can show `path`.
pub fn find<'a>(renderers: &'a [FileRenderer], id: &str, path: &str) -> Option<&'a FileRenderer> {
    renderers.iter().find(|r| r.id() == id && r.accepts(path))
}

/// The first renderer of `renderers` that can show `path`.
fn first<'a>(renderers: &'a [FileRenderer], path: &str) -> Option<&'a FileRenderer> {
    renderers.iter().find(|r| r.accepts(path))
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
/// `picks`, else the first that takes it; `None` for the text editor.
///
/// A pick that cannot show `path` (one made for `flow.diagram.svg` and
/// `logo.svg`, which share a kind, or a renderer since removed) falls
/// back to the first.
pub fn chosen<'a>(
    renderers: &'a [FileRenderer],
    picks: &BTreeMap<String, String>,
    path: &str,
) -> Option<&'a FileRenderer> {
    match picks.get(&file_kind(path)).map(String::as_str) {
        Some(TEXT) => None,
        Some(id) => find(renderers, id, path).or_else(|| first(renderers, path)),
        None => first(renderers, path),
    }
}

/// Whether `path` has one of `extensions` (lowercase, without the dot).
pub fn has_extension(path: &str, extensions: &[&str]) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| extensions.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// A renderer that edits the file (a web editor) changed its text to this.
#[derive(Clone, Debug)]
pub struct TextEdit(pub String);

/// The view of a renderer. Everything but the text is optional; a view
/// that cannot show a line or a comment leaves the defaults, and the file
/// then opens in the text editor for it. A view that edits the file emits
/// [`TextEdit`]; the viewer saves it as an edit in the text editor.
pub trait RendererView:
    Render + EventEmitter<CommentChange> + EventEmitter<TextEdit> + Sized
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
    /// Pass the view's comment changes on as the viewer's, and its edits to
    /// the viewer.
    fn forward_events(&self, cx: &mut Context<FileViewer>) -> Vec<Subscription>;
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

    fn forward_events(&self, cx: &mut Context<FileViewer>) -> Vec<Subscription> {
        vec![
            cx.subscribe(self, |_, _, change: &CommentChange, cx| {
                cx.emit(change.clone())
            }),
            cx.subscribe(self, |viewer, _, edit: &TextEdit, cx| {
                viewer.renderer_edited(edit.0.clone(), cx)
            }),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::custom_renderers::{CommandRenderer, OutputFormat};

    fn built_in() -> Vec<FileRenderer> {
        BUILT_IN.iter().map(FileRenderer::BuiltIn).collect()
    }

    fn custom(label: &str, files: &[&str]) -> FileRenderer {
        FileRenderer::Custom(Arc::new(CustomRenderer {
            label: label.into(),
            files: files.iter().map(|f| f.to_string()).collect(),
            kind: RendererKind::Command(CommandRenderer {
                command: "true".into(),
                output: OutputFormat::Svg,
            }),
        }))
    }

    #[test]
    fn renderers_are_offered_by_extension() {
        let renderers = built_in();
        let ids = |path| {
            for_path(&renderers, path)
                .map(|r| r.id())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("docs/plan.md"), ["markdown"]);
        assert_eq!(ids("README.MARKDOWN"), ["markdown"]);
        assert_eq!(ids("art/logo.svg"), ["svg"]);
        assert_eq!(ids("docs/flow.drawio.svg"), ["svg"]);
        assert!(ids("src/lib.rs").is_empty());
        assert!(ids("Makefile").is_empty());
        assert!(find(&renderers, "markdown", "a.md").is_some());
        assert!(find(&renderers, "markdown", "a.rs").is_none());
    }

    #[test]
    fn a_pick_holds_for_its_kind_of_file() {
        let renderers = built_in();
        let id = |picks: &BTreeMap<String, String>, path| {
            chosen(&renderers, picks, path).map(|r| r.id())
        };
        let mut picks = BTreeMap::new();
        assert_eq!(
            id(&picks, "a.md").as_deref(),
            Some("markdown"),
            "rendered by default"
        );
        assert_eq!(id(&picks, "a.rs"), None);
        picks.insert(file_kind("docs/B.MD"), TEXT.to_owned());
        assert_eq!(id(&picks, "a.md"), None, "text picked for .md");
        assert_eq!(id(&picks, "a.markdown").as_deref(), Some("markdown"));
        assert_eq!(file_kind("Makefile"), "makefile");
    }

    #[test]
    fn custom_renderers_come_first_and_picks_fall_back() {
        let mut renderers = vec![custom("Diagram", &["*.diagram.svg", "*.diagram"])];
        renderers.extend(built_in());
        let ids = |path| {
            for_path(&renderers, path)
                .map(|r| r.id())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("flow.diagram.svg"), ["custom:Diagram", "svg"]);
        assert_eq!(ids("flow.diagram"), ["custom:Diagram"]);
        assert_eq!(ids("logo.svg"), ["svg"]);

        // A pick for `.svg` made on a `.diagram.svg` file leaves other
        // SVGs drawn.
        let mut picks = BTreeMap::new();
        picks.insert(file_kind("flow.diagram.svg"), "custom:Diagram".to_owned());
        let id = |path| chosen(&renderers, &picks, path).map(|r| r.id());
        assert_eq!(id("flow.diagram.svg").as_deref(), Some("custom:Diagram"));
        assert_eq!(id("logo.svg").as_deref(), Some("svg"));
        picks.insert("diagram".into(), "custom:removed".into());
        assert_eq!(
            chosen(&renderers, &picks, "a.diagram")
                .map(|r| r.id())
                .as_deref(),
            Some("custom:Diagram")
        );
    }

    #[test]
    fn a_changed_command_is_another_renderer() {
        let a = custom("x", &["*.x"]);
        let FileRenderer::Custom(inner) = &a else {
            unreachable!()
        };
        let mut changed = (**inner).clone();
        changed.kind = RendererKind::Web(Default::default());
        assert!(a == custom("x", &["*.x"]));
        assert!(a != FileRenderer::Custom(Arc::new(changed)));
        assert!(built_in()[0] == built_in()[0]);
    }

    #[test]
    fn renderer_ids_are_unique_and_not_text() {
        let mut ids: Vec<_> = BUILT_IN.iter().map(|r| r.id).collect();
        assert!(!ids.contains(&TEXT));
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), BUILT_IN.len());
    }
}
