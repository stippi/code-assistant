//! The state a code view keeps around its files ([`CodeList`]) and what
//! it asks of the view holding it ([`CodeHost`]): the files themselves,
//! their "Viewed" ticks, what goes below their rows. The operations here
//! (fold, change the diff options, reveal lines, rebuild the rows) are
//! the same for every host.

use super::anchors::{Annotation, LineRef, place};
use super::context::Reveal;
use super::file::{DiffView, LoadedFile};
use super::folds::{Folds, viewed_range};
use super::preferences::{CodePreferences, DiffLayout};
use super::rows::{ChunkOutline, CodeRow, FileBody, FileOutline, changed_span, flatten};
use crate::comments::AnchorTracker;
use crate::main_screen::right_panel::line_selection::{LineSelection, Selected, SelectsLines};
use gpui_kit::{
    AnyElement, App, Bounds, Context, FocusHandle, IntoElement, ListAlignment, ListOffset,
    ListState, Pixels, UniformListScrollHandle, px,
};
use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

pub use super::draft::Draft;

/// How far beyond the viewport the list builds rows.
const LIST_OVERDRAW: Pixels = px(512.);

/// What stays in view while every row is rebuilt.
#[derive(Clone, Copy, Debug)]
enum KeepInView {
    /// The same row (the rows are the same, only their heights change).
    Row(usize),
    /// The header of the file at the top (the rows change).
    File(usize),
}

/// A file's "Viewed" tick as its header shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewedMark {
    pub viewed: bool,
    /// It changed since it was ticked.
    pub stale: bool,
}

/// The state shared by the code views.
pub struct CodeList {
    /// Prefix of element ids and debug selectors (`pr-code`, `review`).
    pub prefix: &'static str,
    /// Layout, wrap, whitespace, tests, file tree (remembered).
    pub code: CodePreferences,
    pub folds: Folds,
    /// Per shown file: what goes below which of its rows.
    pub annotations: Vec<BTreeMap<usize, Vec<Annotation>>>,
    pub rows: Vec<CodeRow>,
    pub list_state: ListState,
    /// `rows` needs rebuilding.
    pub dirty: bool,
    /// Where the list goes once its rows are rebuilt after a change of
    /// the diff options, so the file in view stays in view.
    keep_in_view: Option<KeepInView>,
    next_stamp: u64,

    /// The file tree's collapsed folders and the file last picked there.
    pub tree_collapsed: HashSet<String>,
    pub tree_selected: Option<String>,
    pub tree_scroll: UniformListScrollHandle,
    /// The toolbar's width at the last paint: narrow, it folds the diff
    /// options into a menu.
    pub toolbar_width: Rc<Cell<Pixels>>,

    /// The last file whose checkbox was clicked: a shift-click ticks from it.
    pub last_ticked: Option<String>,

    pub selection: LineSelection<String>,
    /// The press came from a "+": releasing it opens the comment box.
    pub comment_on_release: bool,
    /// Window bounds of the selection's last row, written when painted.
    pub selection_anchor: Rc<Cell<Option<Bounds<Pixels>>>>,
    pub anchor: AnchorTracker,
    /// The selection whose pill a press elsewhere dismissed.
    pub pill_dismissed: Option<Selected<String>>,
    pub draft: Option<Draft>,

    pub focus_handle: FocusHandle,
}

impl CodeList {
    pub fn new(prefix: &'static str, code: CodePreferences, focus_handle: FocusHandle) -> Self {
        Self {
            prefix,
            code,
            folds: Folds::default(),
            annotations: Vec::new(),
            rows: Vec::new(),
            list_state: ListState::new(0, ListAlignment::Top, LIST_OVERDRAW).measure_all(),
            dirty: true,
            keep_in_view: None,
            next_stamp: 0,
            tree_collapsed: HashSet::new(),
            tree_selected: None,
            tree_scroll: UniformListScrollHandle::new(),
            toolbar_width: Rc::new(Cell::new(px(f32::MAX))),
            last_ticked: None,
            selection: LineSelection::default(),
            comment_on_release: false,
            selection_anchor: Rc::default(),
            anchor: AnchorTracker::default(),
            pill_dismissed: None,
            draft: None,
            focus_handle,
        }
    }

    /// A new name for a file's rows.
    pub fn stamp(&mut self) -> u64 {
        self.next_stamp += 1;
        self.next_stamp
    }

    pub fn diff_view(&self) -> DiffView {
        DiffView {
            hide_whitespace: self.code.hide_whitespace,
            layout: self.code.layout,
        }
    }

    /// Keep what is at the top of the list there through the next rebuild.
    pub fn keep_top(&mut self, same_rows: bool) {
        let top = self.list_state.logical_scroll_top().item_ix;
        self.keep_in_view = match same_rows {
            true => Some(KeepInView::Row(top)),
            false => self
                .rows
                .get(top)
                .and_then(CodeRow::file)
                .map(KeepInView::File),
        };
    }

    /// Start over with no rows (a new scope, another repository).
    pub fn reset(&mut self) {
        self.selection.clear();
        self.draft = None;
        self.annotations.clear();
        self.rows.clear();
        self.list_state.reset(0);
        self.dirty = true;
    }

    /// Whether the chunk of `path` from flat line `base` holds the
    /// selection's last row.
    pub fn anchors_selection_end(&self, path: &str, base: usize, rows: usize) -> bool {
        self.selection.get().is_some_and(|sel| {
            let (_, hi) = sel.range();
            sel.key == path && (base..base + rows).contains(&hi)
        })
    }

    /// The shown files to tick when the checkbox of `ix` was clicked
    /// (with `extend`, from the last clicked one); remembers `path` as the
    /// last clicked.
    pub fn ticked_range(
        &mut self,
        ix: usize,
        path: &str,
        extend: bool,
        shown_ix: impl Fn(&str) -> Option<usize>,
    ) -> Range<usize> {
        let anchor = extend
            .then(|| self.last_ticked.as_deref().and_then(&shown_ix))
            .flatten();
        self.last_ticked = Some(path.to_owned());
        viewed_range(ix, anchor)
    }
}

/// What a code view provides around its [`CodeList`].
pub trait CodeHost: SelectsLines<Key = String> + Sized + 'static {
    fn list(&self) -> &CodeList;
    fn list_mut(&mut self) -> &mut CodeList;

    /// The files in the list, in order.
    fn shown_count(&self) -> usize;
    fn shown_file(&self, ix: usize) -> &LoadedFile;
    fn shown_file_mut(&mut self, ix: usize) -> &mut LoadedFile;
    /// Every file held, shown or not, and the list beside them.
    fn files_and_list(&mut self) -> (Vec<&mut LoadedFile>, &mut CodeList);

    /// The shown file at `path`, by its index in the list.
    fn shown_ix(&self, path: &str) -> Option<usize> {
        (0..self.shown_count()).find(|&ix| self.shown_file(ix).path == path)
    }

    /// Which files are shown, in which order (after hiding tests changed).
    fn sync_files(&mut self, cx: &mut Context<Self>);
    /// Test files among the held ones (shown or hidden).
    fn test_files(&self) -> usize;

    fn viewed(&self, path: &str) -> ViewedMark;
    /// The checkbox of the shown file `ix` was clicked; with `extend`
    /// (shift) every file from the last clicked one is ticked alike.
    fn set_viewed(&mut self, ix: usize, viewed: bool, extend: bool, cx: &mut Context<Self>);

    /// Put what goes below the rows in place (after the comment box opened
    /// or closed); hosts call [`place_annotations`] with their items.
    fn place_annotations(&mut self, cx: &mut Context<Self>);

    /// Read the shown file's text to reveal lines around its hunks; the
    /// host calls [`text_read`] with it.
    fn read_text(&mut self, _ix: usize, _cx: &mut Context<Self>) {}
    fn text_loading(&self, _path: &str) -> bool {
        false
    }

    /// Rows can be commented on.
    fn can_comment(&self) -> bool {
        true
    }
    /// Where a new comment can go: the agent always, the host when it
    /// keeps comments of its own (the primary target then).
    fn comment_host_target(&self) -> Option<&'static str> {
        None
    }
    fn add_comment_to_host(&mut self, _cx: &mut Context<Self>) {}
    /// The file a line comment for the agent names.
    fn comment_file(&self, path: &str) -> PathBuf {
        PathBuf::from(path)
    }
    /// Flat lines `lines` of `file` that a comment is on.
    fn comment_marks(&self, file: &LoadedFile, lines: Range<usize>, cx: &App) -> Vec<Range<usize>>;

    /// What the header shows before the checkbox: `+A −D` by default.
    fn render_header_stat(&self, ix: usize, cx: &App) -> Option<AnyElement> {
        let file = self.shown_file(ix);
        crate::main_screen::right_panel::pull_request::presentation::diff_stat(
            file.additions,
            file.deletions,
            cx,
        )
    }
    /// The note in place of a diff: binary, too large, no change.
    fn note_text(&self, file: &LoadedFile) -> &'static str {
        if file.diff.is_binary {
            "Binary file not shown."
        } else if file.diff.too_large {
            "Too large to show."
        } else if file.orig_path.is_some() {
            "Renamed without changes."
        } else {
            "No content changes."
        }
    }

    /// Version of review thread `thread`, for the list's change check.
    fn thread_version(&self, _thread: usize) -> u64 {
        0
    }
    /// A thread or pending comment row.
    fn render_annotation(&mut self, row: &CodeRow, cx: &mut Context<Self>) -> AnyElement;
    /// More files follow: the list ends in a footer.
    fn has_footer(&self) -> bool {
        false
    }
    fn render_footer(&mut self, _cx: &mut Context<Self>) -> AnyElement {
        gpui_kit::div().into_any_element()
    }
    /// Below the file tree (load more files).
    fn render_tree_footer(&mut self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        None
    }
}

/// Change the remembered diff preferences.
pub fn change_code<V: CodeHost>(
    this: &mut V,
    cx: &mut Context<V>,
    change: impl FnOnce(&mut CodePreferences),
) {
    let list = this.list_mut();
    change(&mut list.code);
    let code = list.code;
    super::preferences::update(cx, |preferences| *preferences = code);
}

/// Every file gets new rows (their heights change).
pub fn restamp<V: CodeHost>(this: &mut V) {
    let (files, list) = this.files_and_list();
    for file in files {
        file.stamp = list.stamp();
    }
    list.dirty = true;
}

pub fn set_layout<V: CodeHost>(this: &mut V, layout: DiffLayout, cx: &mut Context<V>) {
    if layout == this.list().code.layout {
        return;
    }
    change_code(this, cx, |code| code.layout = layout);
    this.list_mut().keep_top(false);
    for file in this.files_and_list().0 {
        let breaks = std::mem::take(&mut file.breaks);
        file.cut(&breaks, layout);
    }
    restamp(this);
    cx.notify();
}

pub fn set_wrap<V: CodeHost>(this: &mut V, wrap: bool, cx: &mut Context<V>) {
    change_code(this, cx, |code| code.wrap = wrap);
    this.list_mut().keep_top(true);
    restamp(this);
    cx.notify();
}

/// Like T3 Code, the selection and an open comment box go: their rows
/// change.
pub fn set_hide_whitespace<V: CodeHost>(this: &mut V, hide: bool, cx: &mut Context<V>) {
    change_code(this, cx, |code| code.hide_whitespace = hide);
    this.list_mut().keep_top(false);
    let view = this.list().diff_view();
    for file in this.files_and_list().0 {
        file.reshape(view);
    }
    restamp(this);
    let list = this.list_mut();
    list.selection.clear();
    list.draft = None;
    this.place_annotations(cx);
}

pub fn set_hide_tests<V: CodeHost>(this: &mut V, hide: bool, cx: &mut Context<V>) {
    change_code(this, cx, |code| code.hide_tests = hide);
    this.sync_files(cx);
    cx.notify();
}

pub fn set_file_tree<V: CodeHost>(this: &mut V, open: bool, cx: &mut Context<V>) {
    change_code(this, cx, |code| code.file_tree = open);
    cx.notify();
}

// -- Folds -----------------------------------------------------------------

pub fn toggle_fold<V: CodeHost>(this: &mut V, ix: usize, cx: &mut Context<V>) {
    if ix < this.shown_count() {
        let path = this.shown_file(ix).path.clone();
        let list = this.list_mut();
        list.folds.toggle(&path);
        list.dirty = true;
        cx.notify();
    }
}

pub fn all_folded<V: CodeHost>(this: &V) -> bool {
    this.list()
        .folds
        .all_folded((0..this.shown_count()).map(|ix| this.shown_file(ix).path.as_str()))
}

pub fn fold_all<V: CodeHost>(this: &mut V, folded: bool, cx: &mut Context<V>) {
    let list = this.list_mut();
    list.folds.set_all(folded);
    list.dirty = true;
    cx.notify();
}

// -- Context ---------------------------------------------------------------

/// A click on a gap of the shown file `ix`: reveal its lines, once the
/// file's text is read (queued until then).
pub fn reveal<V: CodeHost>(
    this: &mut V,
    ix: usize,
    gap: usize,
    reveal: Reveal,
    cx: &mut Context<V>,
) {
    if ix >= this.shown_count() {
        return;
    }
    let file = this.shown_file_mut(ix);
    if file.text.is_none() {
        file.queued_reveals.push((gap, reveal));
        this.read_text(ix, cx);
        return;
    }
    file.revealed.entry(gap).or_default().apply(reveal);
    file_reshaped(this, ix, cx);
}

/// The text of the shown file `ix` arrived: the reveals queued meanwhile
/// apply.
pub fn text_read<V: CodeHost>(
    this: &mut V,
    ix: usize,
    text: super::file::FileText,
    cx: &mut Context<V>,
) {
    let file = this.shown_file_mut(ix);
    file.text = Some(text);
    for (gap, reveal) in std::mem::take(&mut file.queued_reveals) {
        file.revealed.entry(gap).or_default().apply(reveal);
    }
    file_reshaped(this, ix, cx);
}

/// The shown file changed shape: new rows, annotations placed anew.
pub fn file_reshaped<V: CodeHost>(this: &mut V, ix: usize, cx: &mut Context<V>) {
    let view = this.list().diff_view();
    let stamp = this.list_mut().stamp();
    let file = this.shown_file_mut(ix);
    file.reshape(view);
    file.stamp = stamp;
    this.place_annotations(cx);
}

// -- File tree -------------------------------------------------------------

/// Show the shown file `ix` from its header (unfolded), as a click in the
/// file tree asks.
pub fn reveal_file<V: CodeHost>(this: &mut V, ix: usize, cx: &mut Context<V>) {
    if ix >= this.shown_count() {
        return;
    }
    let path = this.shown_file(ix).path.clone();
    let list = this.list_mut();
    if list.folds.is_folded(&path) {
        list.folds.set(&path, false);
        list.dirty = true;
    }
    sync_rows(this);
    let list = this.list_mut();
    if let Some(row) = list
        .rows
        .iter()
        .position(|row| *row == CodeRow::File { file: ix })
    {
        list.list_state.scroll_to(ListOffset {
            item_ix: row,
            offset_in_item: px(0.),
        });
    }
    list.tree_selected = Some(path);
    cx.notify();
}

pub fn toggle_folder<V: CodeHost>(this: &mut V, path: String, cx: &mut Context<V>) {
    let list = this.list_mut();
    if !list.tree_collapsed.remove(&path) {
        list.tree_collapsed.insert(path);
    }
    cx.notify();
}

/// Collapse every folder, or expand them all when all are collapsed.
pub fn toggle_all_folders<V: CodeHost>(this: &mut V, cx: &mut Context<V>) {
    let paths: Vec<String> = (0..this.shown_count())
        .map(|ix| this.shown_file(ix).path.clone())
        .collect();
    let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
    let folders = super::tree::folder_paths(&paths);
    let list = this.list_mut();
    if !folders.is_empty() && folders.is_subset(&list.tree_collapsed) {
        list.tree_collapsed.clear();
    } else {
        list.tree_collapsed = folders;
    }
    cx.notify();
}

// -- Annotations -----------------------------------------------------------

/// Put `items` (per shown file, the host's annotations by line) and the
/// comment box below their rows, and cut the chunks there. Returns per
/// shown file the annotations no row shows.
pub fn place_annotations<V: CodeHost>(
    this: &mut V,
    mut items: Vec<Vec<(LineRef, Annotation)>>,
    cx: &mut Context<V>,
) -> Vec<Vec<Annotation>> {
    let count = this.shown_count();
    items.resize_with(count, Vec::new);
    let mut annotations = Vec::with_capacity(count);
    let mut off = Vec::with_capacity(count);
    for (ix, mut items) in items.into_iter().enumerate() {
        let file = this.shown_file(ix);
        if let Some(draft) = this.list().draft.as_ref().filter(|d| d.path == file.path) {
            items.push((draft.at.end, Annotation::Draft));
        }
        let (placed, missed) = place(&file.lines, items);
        let breaks: Vec<usize> = placed.keys().copied().collect();
        if file.breaks != breaks {
            let stamp = this.list_mut().stamp();
            let layout = this.list().code.layout;
            let file = this.shown_file_mut(ix);
            file.cut(&breaks, layout);
            file.stamp = stamp;
        }
        annotations.push(placed);
        off.push(missed);
    }
    let list = this.list_mut();
    list.annotations = annotations;
    list.dirty = true;
    cx.notify();
    off
}

// -- List ------------------------------------------------------------------

/// Bring the rows and the list in step with the state: one splice.
pub fn sync_rows<V: CodeHost>(this: &mut V) {
    if !this.list().dirty {
        return;
    }
    let host = &*this;
    let list = host.list();
    let thread_version = |ix: usize| host.thread_version(ix);
    let empty = BTreeMap::new();
    let outlines: Vec<FileOutline> = (0..host.shown_count())
        .map(|ix| {
            let file = host.shown_file(ix);
            let annotations = list.annotations.get(ix).unwrap_or(&empty);
            let diff = &file.diff;
            let body = if !file.loaded {
                FileBody::Pending
            } else if diff.is_binary || diff.too_large || diff.hunks.is_empty() {
                FileBody::Note
            } else {
                let chunks = match &file.split {
                    Some(split) => split
                        .chunks
                        .iter()
                        .map(|chunk| ChunkOutline {
                            hunk: chunk.hunk,
                            starts_hunk: chunk.rows.start == 0,
                            last_lines: split.last_lines(chunk),
                        })
                        .collect(),
                    None => diff
                        .chunked
                        .chunks
                        .iter()
                        .map(|chunk| ChunkOutline {
                            hunk: chunk.hunk,
                            starts_hunk: chunk.lines.start == 0,
                            last_lines: vec![
                                chunk.flat_start + chunk.lines.len().saturating_sub(1),
                            ],
                        })
                        .collect(),
                };
                FileBody::Diff {
                    hunk_count: diff.hunks.len(),
                    chunks,
                    gaps: &file.gaps,
                    annotations,
                    thread_version: &thread_version,
                }
            };
            FileOutline {
                folded: list.folds.is_folded(&file.path),
                stamp: file.stamp,
                body,
            }
        })
        .collect();
    let rows = flatten(&outlines, host.has_footer());
    let list = this.list_mut();
    list.dirty = false;
    if let Some((old, count)) = changed_span(&list.rows, &rows) {
        list.list_state.splice(old, count);
    }
    let keep = match list.keep_in_view.take() {
        Some(KeepInView::Row(ix)) => Some(ix.min(rows.len().saturating_sub(1))),
        Some(KeepInView::File(file)) => rows.iter().position(|r| *r == CodeRow::File { file }),
        None => None,
    };
    if let Some(item_ix) = keep {
        list.list_state.scroll_to(ListOffset {
            item_ix,
            offset_in_item: px(0.),
        });
    }
    list.rows = rows;
}
