//! Other ways than the text editor to show a file in the Files view, such as
//! rendered Markdown or a drawn SVG.
//!
//! A renderer is an entry in [`RENDERERS`]: an id, a label for the viewer's
//! dropdown, which files it takes and how to create its view. The view is an
//! entity implementing [`RendererView`]. The [`FileViewer`] keeps loading,
//! editing and saving the file; a renderer gets the file's current text and
//! the draft's comments and reports comments made in it. The text editor is
//! always offered as [`TEXT`] and is not an entry here.
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

use super::file_viewer::FileViewer;
use crate::comments::CommentChange;
use code_assistant_core::line_comments::LineComment;
use gpui_kit::{AnyView, App, Context, Entity, EventEmitter, Render, Subscription, Window};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

/// Every renderer besides the text editor.
pub const RENDERERS: &[FileRenderer] = &[markdown::RENDERER, svg::RENDERER];

/// The renderers that can show `path`, besides the text editor.
pub fn for_path(path: &str) -> impl Iterator<Item = &'static FileRenderer> + '_ {
    RENDERERS.iter().filter(move |r| (r.accepts)(path))
}

/// The renderer with `id`, when it can show `path`.
pub fn find(id: &str, path: &str) -> Option<&'static FileRenderer> {
    for_path(path).find(|r| r.id == id)
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
pub fn chosen(picks: &BTreeMap<String, String>, path: &str) -> Option<&'static FileRenderer> {
    match picks.get(&file_kind(path)) {
        Some(id) => find(id, path),
        None => for_path(path).next(),
    }
}

/// Whether `path` has one of `extensions` (lowercase, without the dot).
pub fn has_extension(path: &str, extensions: &[&str]) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| extensions.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// The view of a renderer. Everything but the text is optional; a view
/// that cannot show a line or a comment leaves the defaults, and the file
/// then opens in the text editor for it.
pub trait RendererView: Render + EventEmitter<CommentChange> + Sized {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderers_are_offered_by_extension() {
        let ids = |path| for_path(path).map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(ids("docs/plan.md"), ["markdown"]);
        assert_eq!(ids("README.MARKDOWN"), ["markdown"]);
        assert_eq!(ids("art/logo.svg"), ["svg"]);
        assert_eq!(ids("docs/flow.drawio.svg"), ["svg"]);
        assert!(ids("src/lib.rs").is_empty());
        assert!(ids("Makefile").is_empty());
        assert!(find("markdown", "a.md").is_some());
        assert!(find("markdown", "a.rs").is_none());
    }

    #[test]
    fn a_pick_holds_for_its_kind_of_file() {
        let id = |picks: &BTreeMap<String, String>, path| chosen(picks, path).map(|r| r.id);
        let mut picks = BTreeMap::new();
        assert_eq!(id(&picks, "a.md"), Some("markdown"), "rendered by default");
        assert_eq!(id(&picks, "a.rs"), None);
        picks.insert(file_kind("docs/B.MD"), TEXT.to_owned());
        assert_eq!(id(&picks, "a.md"), None, "text picked for .md");
        assert_eq!(id(&picks, "a.markdown"), Some("markdown"));
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
