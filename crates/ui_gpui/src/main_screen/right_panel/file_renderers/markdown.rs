//! Markdown files rendered, as in the chat.
//!
//! Text can be selected; releasing the mouse over a selection shows the
//! selection pill (copy, comment). A comment covers the source lines the
//! selection was rendered from, so it is an ordinary comment on the file's
//! lines: the text editor marks the same lines, and the model gets them with
//! their line numbers. Commented passages are tinted.
//!
//! Relative links (and links from the project root, `/docs/a.md`) open the
//! linked file in the Files view; other links open in the browser.

use super::{BuiltIn, RendererHandle, RendererView, TextEdit, has_extension};
use crate::comments::editor::{CommentEditor, CommentEditorEvent};
use crate::comments::{AnchorTracker, CommentChange, floating, selection_pill};
use crate::shared::open_file::{self, OpenFileRequest};
use code_assistant_core::line_comments::{self, LineComment};
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::text::{
    RangeHighlight, RenderedText, SelectionFormat, TextView, TextViewState,
};
use gpui_kit::{
    AnyElement, App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, MouseButton,
    MouseUpEvent, Pixels, Point, Render, Subscription, Window, canvas, div, point, prelude::*, px,
};
use std::cell::Cell;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

pub const RENDERER: BuiltIn = BuiltIn {
    id: "markdown",
    label: "Markdown",
    accepts: |path| has_extension(path, &["md", "markdown", "mdx"]),
    create,
};

fn create(
    file: PathBuf,
    root: PathBuf,
    _window: &mut Window,
    cx: &mut App,
) -> Box<dyn RendererHandle> {
    Box::new(cx.new(|cx| MarkdownPreview::new(file, root, cx)))
}

/// Where a card opened from outside (the composer's list) floats, relative
/// to the view.
const REVEAL_CARD_OFFSET: Point<Pixels> = Point {
    x: px(16.),
    y: px(16.),
};

/// A selection the pill is shown for.
struct Selection {
    /// Where the mouse was released, relative to the view.
    offset: Point<Pixels>,
    /// The Markdown source the selection was rendered from.
    source: Range<usize>,
}

pub struct MarkdownPreview {
    file: PathBuf,
    /// The project root, for links from it.
    root: PathBuf,
    source: String,
    state: Entity<TextViewState>,
    /// The draft's comments on this file.
    comments: Vec<LineComment>,
    /// The rendered text the marks were last computed for; `None` when they
    /// need computing again.
    marked: Option<RenderedText>,
    /// A line (1-based) to scroll to once the text is rendered.
    pending_line: Option<usize>,
    selection: Option<Selection>,
    /// The open comment card and where it floats, relative to the view.
    comment_editor: Option<(Entity<CommentEditor>, Point<Pixels>)>,
    _comment_editor_subscription: Option<Subscription>,
    /// The view's origin in the last frame, in window coordinates.
    origin: Rc<Cell<Option<Point<Pixels>>>>,
    anchor: AnchorTracker,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CommentChange> for MarkdownPreview {}
impl EventEmitter<TextEdit> for MarkdownPreview {}

impl MarkdownPreview {
    fn new(file: PathBuf, root: PathBuf, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| TextViewState::markdown("", cx));
        // A parse landing changes the rendered text the marks index.
        let subscriptions = vec![cx.observe(&state, |this, _, cx| this.on_rendered(cx))];
        Self {
            file,
            root,
            source: String::new(),
            state,
            comments: Vec::new(),
            marked: None,
            pending_line: None,
            selection: None,
            comment_editor: None,
            _comment_editor_subscription: None,
            origin: Rc::default(),
            anchor: AnchorTracker::default(),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// The rendered text, once it is rendered from the current source.
    fn rendered(&self, cx: &App) -> Option<RenderedText> {
        let rendered = self.state.read(cx).rendered_text();
        (rendered.source() == self.source).then_some(rendered)
    }

    fn on_rendered(&mut self, cx: &mut Context<Self>) {
        // Most notifications (selection drags, the highlights set below)
        // change nothing the marks depend on; telling that by the snapshot
        // spares comparing the whole source.
        if self.pending_line.is_none()
            && self.marked.as_ref() == Some(&self.state.read(cx).rendered_text())
        {
            return;
        }
        let Some(rendered) = self.rendered(cx) else {
            return;
        };
        if let Some(line) = self.pending_line.take() {
            // A line that renders nothing (blank, a fence) reveals the first
            // text rendered after it.
            let range = rendered
                .range_for_source(line_range(&self.source, line, line))
                .or_else(|| {
                    rendered
                        .range_for_source(line_range(&self.source, line, usize::MAX))
                        .map(|r| r.start..r.start)
                });
            if let Some(range) = range {
                self.state
                    .update(cx, |state, cx| state.reveal_range(range, cx).ok());
            }
        }
        // Setting the highlights notifies the state again; the rendered text
        // is the same then, so nothing is computed twice.
        if self.marked.as_ref() != Some(&rendered) {
            self.mark(rendered, cx);
        }
    }

    /// Tint the commented passages.
    fn mark(&mut self, rendered: RenderedText, cx: &mut Context<Self>) {
        let lines: Vec<&str> = self.source.lines().collect();
        let color = crate::comments::mark_color(cx);
        let highlights: Vec<RangeHighlight> = self
            .comments
            .iter()
            .filter_map(|comment| {
                let (start, end) = line_comments::locate(&lines, comment)?;
                let range = rendered.range_for_source(line_range(&self.source, start, end))?;
                Some(RangeHighlight::new(range, color))
            })
            .collect();
        self.state.update(cx, |state, cx| {
            if let Err(e) = state.set_range_highlights(highlights, cx) {
                tracing::debug!("Markdown view: comment marks rejected: {e:?}");
            }
        });
        self.marked = Some(rendered);
    }

    /// The mouse was released over the text: show the pill there when text
    /// is selected now.
    fn on_mouse_up(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.selection =
            self.current_selection(cx)
                .zip(self.origin.get())
                .map(|(source, origin)| Selection {
                    offset: position - origin,
                    source,
                });
        cx.notify();
    }

    /// The Markdown source of the selection, while it indexes the current
    /// source (a reload's parse may not have landed yet).
    fn current_selection(&self, cx: &App) -> Option<Range<usize>> {
        self.rendered(cx)?;
        self.state
            .read(cx)
            .selected_source_range()
            .filter(|r| !r.is_empty())
    }

    fn copy_selection(&self, cx: &mut App) {
        let text = self.state.read(cx).selected_text();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// The comment for the selection: an existing one on its lines, or a
    /// new one.
    fn comment_for(&self, source: Range<usize>) -> LineComment {
        let (start, end) = lines_of(&self.source, source);
        if let Some(existing) = self.comments.iter().find(|c| c.overlaps(start, end)) {
            return existing.clone();
        }
        let range = line_range(&self.source, start, end);
        LineComment::on_lines(self.file.clone(), start, end, &self.source[range])
    }

    /// Open the comment card for the selection the pill was shown for.
    fn comment_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selection) = self.selection.take() else {
            return;
        };
        let comment = self.comment_for(selection.source);
        self.open_comment_editor(comment, selection.offset, window, cx);
    }

    fn open_comment_editor(
        &mut self,
        comment: LineComment,
        offset: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = cx.new(|cx| CommentEditor::new(comment, window, cx));
        self._comment_editor_subscription = Some(cx.subscribe_in(
            &editor,
            window,
            |this, _, event: &CommentEditorEvent, window, cx| {
                match event {
                    CommentEditorEvent::Save(comment) => {
                        cx.emit(CommentChange::Upsert(comment.clone()))
                    }
                    CommentEditorEvent::Delete(id) => cx.emit(CommentChange::Remove(*id)),
                    CommentEditorEvent::Cancel => {}
                }
                this.comment_editor = None;
                this._comment_editor_subscription = None;
                // Back to the text, so the shortcuts work again.
                window.focus(&this.focus_handle, cx);
                cx.notify();
            },
        ));
        self.comment_editor = Some((editor, offset));
        cx.notify();
    }

    /// Open `link`: a relative one as a project file, any other in the
    /// browser.
    fn open_link(file: &Path, root: &Path, link: &str, cx: &mut App) {
        match resolve_link(file, root, link) {
            Some(path) => open_file::request(
                OpenFileRequest {
                    path: path.to_string_lossy().into_owned(),
                    line: None,
                    project: None,
                },
                cx,
            ),
            None if !link.starts_with('#') => cx.open_url(link),
            None => {}
        }
    }

    /// The pill or the card, floating at its place in the view.
    fn render_floating(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let origin = self.origin.get();
        if let Some((editor, offset)) = &self.comment_editor {
            let position = origin.map(|o| o + *offset + point(px(0.), px(6.)));
            self.anchor.track(position, window);
            return position.map(|p| floating(p, editor.clone()));
        }
        let position = origin
            .zip(self.selection.as_ref())
            .map(|(o, s)| o + s.offset + point(px(0.), px(6.)));
        self.anchor.track(position, window);
        let view = cx.entity().downgrade();
        let view_for_comment = view.clone();
        // A press anywhere else dismisses the pill.
        let dismiss = cx.listener(|this, _: &gpui_kit::MouseDownEvent, _, cx| {
            this.selection = None;
            cx.notify();
        });
        Some(floating(
            position?,
            div().on_mouse_down_out(dismiss).child(selection_pill(
                "file-markdown-selection",
                move |_, cx| {
                    view.update(cx, |view, cx| view.copy_selection(cx)).ok();
                },
                move |window, cx| {
                    view_for_comment
                        .update(cx, |view, cx| view.comment_selection(window, cx))
                        .ok();
                },
                cx,
            )),
        ))
    }
}

impl RendererView for MarkdownPreview {
    fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if text == self.source {
            return;
        }
        self.source = text.to_owned();
        self.selection = None;
        self.marked = None;
        self.state.update(cx, |state, cx| state.set_text(text, cx));
        cx.notify();
    }

    fn set_comments(&mut self, comments: &[LineComment], cx: &mut Context<Self>) {
        let mine: Vec<LineComment> = comments
            .iter()
            .filter(|c| !c.in_diff && !c.on_message && c.file == self.file)
            .cloned()
            .collect();
        if mine != self.comments {
            self.comments = mine;
            self.marked = None;
            self.on_rendered(cx);
        }
    }

    fn reveal_line(&mut self, line: usize, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.pending_line = Some(line);
        self.on_rendered(cx);
        true
    }

    fn reveal_comment(
        &mut self,
        comment: LineComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let lines: Vec<&str> = self.source.lines().collect();
        let start = line_comments::locate(&lines, &comment).map_or(comment.start_line, |(s, _)| s);
        self.reveal_line(start, window, cx);
        self.open_comment_editor(comment, REVEAL_CARD_OFFSET, window, cx);
        true
    }

    /// Comment on the selection, also when its pill was dismissed or never
    /// shown (select all).
    fn start_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(source) = self.current_selection(cx) else {
            return;
        };
        let offset = self
            .selection
            .take()
            .map_or(REVEAL_CARD_OFFSET, |s| s.offset);
        let comment = self.comment_for(source);
        self.open_comment_editor(comment, offset, window, cx);
    }
}

impl Render for MarkdownPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let file = self.file.clone();
        let root = self.root.clone();
        let origin = self.origin.clone();
        // A Markdown source copy for selections across styles, plain text
        // within one, as in the chat.
        let text = TextView::new(&self.state)
            .selectable(true)
            .scrollable(true)
            .selection_format(SelectionFormat::SourceAcrossStyles)
            .on_link_click(move |link, _, _, cx| Self::open_link(&file, &root, link, cx));
        div()
            .id("file-markdown")
            .size_full()
            .relative()
            .track_focus(&self.focus_handle)
            .text_color(cx.theme().foreground)
            .child(
                canvas(
                    move |bounds, _, _| origin.set(Some(bounds.origin)),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(div().size_full().pl_3().child(text))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| window.focus(&this.focus_handle, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _, cx| {
                    this.on_mouse_up(event.position, cx)
                }),
            )
            // The pill stays where the mouse was released; scrolling moves
            // the text away from it.
            .on_scroll_wheel(cx.listener(|this, _, _, cx| {
                if this.selection.take().is_some() {
                    cx.notify();
                }
            }))
            .children(self.render_floating(window, cx))
    }
}

/// The lines (1-based, inclusive) `range` of `source` lies on. A range
/// ending at the start of a line does not include it.
fn lines_of(source: &str, range: Range<usize>) -> (usize, usize) {
    // A stale offset may lie past the text or inside a character.
    let before = |offset: usize| {
        let mut end = offset.min(source.len());
        while !source.is_char_boundary(end) {
            end -= 1;
        }
        &source[..end]
    };
    let line_at = |offset: usize| before(offset).matches('\n').count() + 1;
    let start = line_at(range.start);
    let mut end = line_at(range.end);
    if end > start && before(range.end).ends_with('\n') {
        end -= 1;
    }
    (start, end)
}

/// The byte range of lines `start..=end` (1-based) of `source`, without the
/// last line's newline.
fn line_range(source: &str, start: usize, end: usize) -> Range<usize> {
    let line_start = |line: usize| {
        if line <= 1 {
            return 0;
        }
        source
            .match_indices('\n')
            .nth(line - 2)
            .map_or(source.len(), |(i, _)| i + 1)
    };
    let from = line_start(start);
    let to = source[from..]
        .match_indices('\n')
        .nth(end.saturating_sub(start))
        .map_or(source.len(), |(i, _)| from + i);
    from..to.max(from)
}

/// The file a relative `link` in `file` points to, or one from the project
/// `root` (`/docs/a.md`); `None` for links with a scheme (`https:`,
/// `mailto:`) and anchors within the page.
fn resolve_link(file: &Path, root: &Path, link: &str) -> Option<PathBuf> {
    let path = link.split(['#', '?']).next().unwrap_or_default();
    let has_scheme = path
        .split_once(':')
        .is_some_and(|(scheme, _)| !scheme.is_empty() && !scheme.contains('/'));
    if path.is_empty() || has_scheme {
        return None;
    }
    let path = percent_encoding::percent_decode_str(path).decode_utf8_lossy();
    let joined = match path.strip_prefix('/') {
        Some(from_root) => root.join(from_root),
        None => file.parent()?.join(path.as_ref()),
    };
    // `..` and `.` resolved, so the path lies under the project root as a
    // plain prefix.
    let mut normal = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                normal.pop();
            }
            Component::CurDir => {}
            other => normal.push(other),
        }
    }
    Some(normal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;

    const SOURCE: &str = "# Title\n\nFirst line\nsecond line\n\n- item\n";

    #[test]
    fn source_ranges_map_to_lines() {
        let first = SOURCE.find("First").unwrap();
        let second_end = SOURCE.find("second line").unwrap() + "second line".len();
        assert_eq!(lines_of(SOURCE, first..second_end), (3, 4));
        assert_eq!(lines_of(SOURCE, 0..3), (1, 1));
        // Through the newline at the end of line 4: still line 4 only.
        assert_eq!(lines_of(SOURCE, first..second_end + 1), (3, 4));
        // An offset past the text or inside a character does not panic.
        assert_eq!(lines_of("é\nb", 1..99), (1, 2));
    }

    #[test]
    fn lines_map_to_source_ranges() {
        assert_eq!(&SOURCE[line_range(SOURCE, 1, 1)], "# Title");
        assert_eq!(&SOURCE[line_range(SOURCE, 3, 4)], "First line\nsecond line");
        assert_eq!(&SOURCE[line_range(SOURCE, 6, 6)], "- item");
        // Past the end: an empty range at the end.
        assert_eq!(line_range(SOURCE, 20, 20), SOURCE.len()..SOURCE.len());
    }

    #[test]
    fn relative_links_resolve_to_files() {
        let file = Path::new("/p/docs/plan.md");
        let link = |link| resolve_link(file, Path::new("/p"), link);
        assert_eq!(link("../src/lib.rs"), Some(PathBuf::from("/p/src/lib.rs")));
        assert_eq!(
            link("./other.md#section"),
            Some(PathBuf::from("/p/docs/other.md"))
        );
        assert_eq!(
            link("/crates/a%20b/Gr%C3%BC%C3%9Fe.md"),
            Some(PathBuf::from("/p/crates/a b/Grüße.md")),
            "from the root, percent-decoded"
        );
        assert_eq!(link("https://example.com/a"), None);
        assert_eq!(link("mailto:a@b.c"), None);
        assert_eq!(link("#section"), None);
    }

    #[gpui_kit::test]
    fn a_selection_comments_on_its_source_lines(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::theme::init);
        let file = PathBuf::from("/p/docs/plan.md");
        let view = cx
            .update(|cx| cx.new(|cx| MarkdownPreview::new(file.clone(), PathBuf::from("/p"), cx)));
        view.update(cx, |view, cx| view.set_text(SOURCE, cx));
        cx.run_until_parked();

        // From inside "First" to inside "second": both lines, whole.
        let start = SOURCE.find("rst").unwrap();
        let end = SOURCE.find("cond").unwrap();
        let comment = view.read_with(cx, |view, _| view.comment_for(start..end));
        assert_eq!((comment.start_line, comment.end_line), (3, 4));
        assert_eq!(comment.excerpt, "First line\nsecond line");
        assert_eq!(comment.file, file);

        // A comment on those lines is marked, and a selection on them edits
        // it instead of adding one.
        let existing = LineComment {
            id: 7,
            text: "clarify".into(),
            ..comment
        };
        let elsewhere = LineComment {
            file: PathBuf::from("/p/other.md"),
            ..existing.clone()
        };
        view.update(cx, |view, cx| {
            view.set_comments(&[existing.clone(), elsewhere], cx)
        });
        cx.run_until_parked();
        view.read_with(cx, |view, cx| {
            assert_eq!(view.comments, std::slice::from_ref(&existing));
            assert!(view.marked.is_some(), "the parse landed");
            assert_eq!(view.marked, view.rendered(cx), "marks follow the parse");
            let item = SOURCE.find("item").unwrap();
            assert_eq!(view.comment_for(end..end + 1).id, 7);
            assert_eq!(view.comment_for(item..item + 4).id, 0);
        });
    }
}
