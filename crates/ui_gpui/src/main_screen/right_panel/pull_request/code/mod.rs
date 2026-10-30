//! The Code tab: a pull request's changed files (or one commit's) with
//! their diffs, review threads next to their lines and the pending
//! review's comments, modelled on T3 Code's `PullRequestCodeTab`.
//!
//! - **Loading**: the diff page by page (`pull_request_diff`), the first
//!   when the tab is first drawn, the next when the end of the list comes
//!   into view; each page parsed and highlighted once, off the UI thread
//!   ([`pages`]). A re-read (the core announces a changed diff, or the head
//!   moved) keeps unchanged pages and everything built from them.
//! - **Order**: per page by imports and tiers, then as a file tree would
//!   list them ([`file_order`]); tests can be hidden ([`file_kind`]); the
//!   file tree beside the list ([`tree`]) jumps to a file.
//! - **Shape**: stacked or split, wrapped or scrolled sideways, with or
//!   without whitespace changes ([`pages`]); unchanged lines between hunks
//!   revealed from the file's text, read on the first click ([`context`]).
//! - **Viewed**: optimistic ticks, written in batches ([`viewed`]); ticking
//!   folds a file, a shift-click ticks a range ([`folds`]).
//! - **List**: one virtualized list over flat rows ([`rows`]): headers,
//!   gaps, ≤ 40-line chunks of the shared diff layer, threads, pending
//!   comments and the comment box right below their line ([`anchors`]).
//!   State changes reach it as one splice, so heights and the scroll
//!   position survive; rows are rebuilt only when something changed.
//! - **Comments**: a selection of rows → the pill → a box below the last
//!   row → the pending review ([`review_draft`](super::review_draft)) or
//!   the agent's draft ([`comments`]). Not in a commit's scope: GitHub
//!   anchors comments to the whole change.

mod anchors;
mod aside;
mod body;
mod comments;
mod context;
mod file_header;
mod file_kind;
mod file_order;
mod folds;
mod pages;
mod rows;
mod toolbar;
mod tree;
mod viewed;

use super::PullRequestIntent;
use super::bus::{PullRequestEvent, bus};
use super::conversation::thread::ThreadCard;
use super::data::{PullRequestData, session_service};
use super::detail::TabScroll;
use super::preferences::{self, CodePreferences, DiffLayout};
use super::review_draft::{ReviewDrafts, review_drafts};
use crate::comments::AnchorTracker;
use crate::diff::prepare::highlight_patch;
use crate::diff::syntax::DiffSyntax;
use crate::main_screen::right_panel::line_selection::{LineSelection, Selected, SelectsLines};
use crate::main_screen::right_panel::review_rows::changed_span;
use anchors::{Annotation, LineRef, place};
use code_assistant_core::pull_requests::PullRequestPart;
use comments::Draft;
use context::{Reveal, lines_of};
use file_order::order_like_tree;
use folds::{Folds, viewed_range};
use github::PullRequestRef;
use github::model::{Activity, FileChange, ReviewThread};
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    ListAlignment, ListOffset, ListState, Pixels, Render, SharedString, Subscription, Task,
    UniformListScrollHandle, Window, div, list, prelude::*, px,
};
use pages::{Arrival, DiffView, FileText, LoadedFile, Page, arrival, fingerprint, parse_page};
use rows::{ChunkOutline, CodeRow, FileBody, FileOutline, files_by_proximity, flatten};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use viewed::{Batch, ViewedFiles};

/// Presses on "Viewed" wait this long for more before they are written.
const VIEWED_FLUSH_DELAY: Duration = Duration::from_millis(400);

/// How far beyond the viewport the list builds rows; the footer within it
/// asks for the next page.
const LIST_OVERDRAW: Pixels = px(512.);

/// A shown file: its page and its index there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Shown {
    page: usize,
    file: usize,
}

pub struct CodeTab {
    data: Entity<PullRequestData>,
    pr: PullRequestRef,
    /// The commit the diff is scoped to; `None` for all of them.
    pub(super) commit: Option<String>,
    /// Bumped with each new scope; arrivals for an older one are dropped.
    scope: u64,
    /// The first page was asked for (on the first draw).
    started: bool,
    pages: Vec<Page>,
    /// The page read under way.
    loading: Option<Task<()>>,
    /// The file being highlighted.
    highlighting: Option<Task<()>>,
    /// When the tab was first drawn, until its first rows show.
    mounted_at: Option<Instant>,
    load_error: Option<SharedString>,
    next_stamp: u64,

    /// The files in the list, in order.
    shown: Vec<Shown>,
    test_files: usize,
    /// Layout, wrap, whitespace, tests, file tree (remembered).
    code: CodePreferences,
    folds: Folds,
    /// Files' texts being read, by path: one read per file at a time.
    text_reads: HashMap<String, Task<()>>,
    /// Clicks on a gap made before the file's text was read.
    queued_reveals: HashMap<String, Vec<(usize, Reveal)>>,

    /// The file tree's collapsed folders and the file last picked there.
    tree_collapsed: HashSet<String>,
    tree_selected: Option<String>,
    tree_scroll: UniformListScrollHandle,
    /// The toolbar's width at the last paint: narrow, it folds the diff
    /// options into a menu.
    toolbar_width: Rc<Cell<Pixels>>,

    viewed: ViewedFiles,
    viewed_read: Option<Task<()>>,
    viewed_flush: Option<Task<()>>,
    viewed_write: Option<Task<()>>,
    /// The last file whose checkbox was clicked: a shift-click ticks from it.
    last_ticked: Option<String>,

    activity: Option<Arc<Activity>>,
    head_sha: Option<String>,
    /// Per review thread: what its row shows, for the list's change check.
    thread_versions: Vec<u64>,
    /// Per shown file: what goes below which of its rows.
    annotations: Vec<BTreeMap<usize, Vec<Annotation>>>,
    /// Threads no rendered row shows, by index.
    off_diff: Vec<usize>,
    off_diff_open: bool,
    thread_cards: HashMap<String, (Entity<ThreadCard>, Subscription)>,
    /// How many commits the scope menu lists.
    commits_shown: usize,

    rows: Vec<CodeRow>,
    list_state: ListState,
    /// `rows` needs rebuilding.
    dirty: bool,

    selection: LineSelection<String>,
    /// Window bounds of the selection's last row, written when painted.
    selection_anchor: Rc<Cell<Option<Bounds<Pixels>>>>,
    anchor: AnchorTracker,
    /// The selection whose pill a press elsewhere dismissed.
    pill_dismissed: Option<Selected<String>>,
    draft: Option<Draft>,
    drafts: Entity<ReviewDrafts>,

    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PullRequestIntent> for CodeTab {}

impl CodeTab {
    /// How far the tab is scrolled; the header condenses once it is.
    pub fn scroll(&self) -> TabScroll {
        TabScroll::of(&self.list_state)
    }

    pub fn new(data: Entity<PullRequestData>, cx: &mut Context<Self>) -> Self {
        let pr = data.read(cx).pr.clone();
        let events = bus(cx);
        let drafts = review_drafts(cx);
        let subscriptions = vec![
            cx.observe(&data, |this, _, cx| this.sync_data(cx)),
            cx.subscribe(&events, |this, _, event: &PullRequestEvent, cx| {
                if let PullRequestEvent::Changed { pr, part } = event
                    && *pr == this.pr
                    && this.started
                {
                    match part {
                        PullRequestPart::Diff => this.reload(cx),
                        PullRequestPart::FilesViewed => this.read_viewed(cx),
                        _ => {}
                    }
                }
            }),
            cx.observe(&drafts, |this, _, cx| this.place_annotations(cx)),
            // Presses still waiting for their batch go out on the way.
            cx.on_release(|this, cx| this.write_remaining_viewed(cx)),
        ];
        let mut this = Self {
            data,
            pr,
            commit: None,
            scope: 0,
            started: false,
            pages: Vec::new(),
            loading: None,
            highlighting: None,
            mounted_at: None,
            load_error: None,
            next_stamp: 0,
            shown: Vec::new(),
            test_files: 0,
            code: preferences::read(cx).code,
            folds: Folds::default(),
            text_reads: HashMap::new(),
            queued_reveals: HashMap::new(),
            tree_collapsed: HashSet::new(),
            tree_selected: None,
            tree_scroll: UniformListScrollHandle::new(),
            toolbar_width: Rc::new(Cell::new(px(f32::MAX))),
            viewed: ViewedFiles::default(),
            viewed_read: None,
            viewed_flush: None,
            viewed_write: None,
            last_ticked: None,
            activity: None,
            head_sha: None,
            thread_versions: Vec::new(),
            annotations: Vec::new(),
            off_diff: Vec::new(),
            off_diff_open: false,
            thread_cards: HashMap::new(),
            commits_shown: toolbar::COMMIT_PAGE,
            rows: Vec::new(),
            list_state: ListState::new(0, ListAlignment::Top, LIST_OVERDRAW).measure_all(),
            dirty: true,
            selection: LineSelection::default(),
            selection_anchor: Rc::default(),
            anchor: AnchorTracker::default(),
            pill_dismissed: None,
            draft: None,
            drafts,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        this.sync_data(cx);
        this
    }

    // -- Data --------------------------------------------------------------

    /// Take what the page read: threads and commits from the activity; a
    /// new head reads the diff and the ticks again.
    fn sync_data(&mut self, cx: &mut Context<Self>) {
        let (activity, head_sha) = {
            let data = self.data.read(cx);
            let head = data.detail.as_ref().and_then(|d| d.head_sha.clone());
            (data.activity.clone(), head)
        };
        if head_sha.is_some() && head_sha != self.head_sha {
            let moved = self.head_sha.is_some();
            self.head_sha = head_sha;
            if moved && self.started {
                self.reload(cx);
                self.read_viewed(cx);
            }
        }
        let same = match (&activity, &self.activity) {
            (Some(new), Some(old)) => Arc::ptr_eq(new, old),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.activity = activity.clone();
        let threads = activity.as_ref().map_or(&[][..], |a| &a.review_threads[..]);
        self.thread_versions = threads.iter().map(thread_version).collect();
        self.thread_cards
            .retain(|id, _| threads.iter().any(|thread| &thread.id == id));
        for thread in threads {
            if let Some((card, _)) = self.thread_cards.get(&thread.id) {
                card.update(cx, |card, cx| card.set_thread(thread, cx));
            }
        }
        self.place_annotations(cx);
    }

    /// Read the first page and the ticks; called on the first draw.
    fn start(&mut self, cx: &mut Context<Self>) {
        self.started = true;
        self.mounted_at = Some(Instant::now());
        self.load_page(None, cx);
        self.read_viewed(cx);
    }

    /// Read the first page again; the pages after it stay unless it changed.
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.loading = None;
        self.load_error = None;
        self.load_page(None, cx);
    }

    /// Read the page after the last one, unless one is under way or the
    /// last read failed (the footer offers a retry).
    fn load_next(&mut self, cx: &mut Context<Self>) {
        if self.loading.is_some() || self.load_error.is_some() {
            return;
        }
        if let Some(cursor) = self.pages.last().and_then(|p| p.next_cursor.clone()) {
            self.load_page(Some(cursor), cx);
        }
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        self.load_error = None;
        match self.pages.is_empty() {
            true => self.load_page(None, cx),
            false => self.load_next(cx),
        }
        cx.notify();
    }

    fn load_page(&mut self, cursor: Option<String>, cx: &mut Context<Self>) {
        let Some(service) = session_service(cx) else {
            return;
        };
        let (pr, commit, scope) = (self.pr.clone(), self.commit.clone(), self.scope);
        let view = self.diff_view();
        self.loading = Some(cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let result = service.pull_request_diff(pr, commit, cursor.clone()).await;
            tracing::debug!(?cursor, elapsed = ?started.elapsed(), "pull request diff page read");
            let page = match result {
                Ok(fetched) => fetched.value,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        if this.scope == scope {
                            this.loading = None;
                            this.load_error = Some(format!("{error:#}").into());
                            cx.notify();
                        }
                    })
                    .ok();
                    return;
                }
            };
            let print = fingerprint(&page);
            let wanted = this
                .read_with(cx, |this, _| {
                    this.scope == scope
                        && matches!(
                            arrival(&this.pages, cursor.as_deref(), print),
                            Arrival::Place(_)
                        )
                })
                .unwrap_or(false);
            if !wanted {
                this.update(cx, |this, _| this.loading = None).ok();
                return;
            }
            let (next_cursor, truncated) = (page.next_cursor.clone(), page.truncated);
            let parsing = Instant::now();
            let files = cx
                .background_spawn(async move { parse_page(&page, view) })
                .await;
            tracing::debug!(
                files = files.len(),
                elapsed = ?parsing.elapsed(),
                "pull request diff page parsed"
            );
            this.update(cx, |this, cx| {
                if this.scope == scope {
                    this.page_arrived(
                        Page {
                            cursor,
                            fingerprint: print,
                            next_cursor,
                            truncated,
                            files,
                        },
                        cx,
                    );
                }
            })
            .ok();
        }));
    }

    fn page_arrived(&mut self, mut page: Page, cx: &mut Context<Self>) {
        self.loading = None;
        self.load_error = None;
        if let Arrival::Place(ix) = arrival(&self.pages, page.cursor.as_deref(), page.fingerprint) {
            // Shaped off the UI thread for the view then; it may have
            // changed meanwhile.
            let view = self.diff_view();
            for file in &mut page.files {
                file.reshape(view);
                file.stamp = self.stamp();
            }
            self.pages.truncate(ix);
            self.pages.push(page);
            self.sync_files(cx);
        }
        cx.notify();
    }

    /// Highlight the next file without syntax, nearest the view first,
    /// one at a time off the UI thread. The rows show plain until then;
    /// highlighting changes no height, so the list needs no splice.
    fn highlight_next(&mut self, cx: &mut Context<Self>) {
        if self.highlighting.is_some() {
            return;
        }
        self.sync_rows();
        let top = self.list_state.logical_scroll_top().item_ix;
        // A file whose text was read is highlighted from it.
        let Some(shown) = files_by_proximity(&self.rows, self.shown.len(), top)
            .into_iter()
            .map(|ix| self.shown[ix])
            .find(|&shown| {
                let file = self.file(shown);
                !file.highlighted && file.text.is_none()
            })
        else {
            return;
        };
        let file = self.file(shown);
        let (path, hunks) = (file.path.clone(), file.patch_hunks.clone());
        let (page, print, scope) = (shown.page, self.pages[shown.page].fingerprint, self.scope);
        // Captured here: the line styles are computed off the UI thread.
        let theme = cx.theme().highlight_theme.clone();
        self.highlighting = Some(cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let (syntax, path) = cx
                .background_spawn(async move { (highlight_patch(&path, &hunks, &theme), path) })
                .await;
            tracing::debug!(%path, elapsed = ?started.elapsed(), "pull request file highlighted");
            this.update(cx, |this, cx| {
                this.highlighting = None;
                if this.scope == scope
                    && let Some(file) = this
                        .pages
                        .get_mut(page)
                        .filter(|p| p.fingerprint == print)
                        .and_then(|p| p.files.iter_mut().find(|f| f.path == path))
                {
                    // It fits the hunks without whitespace changes too:
                    // each side keeps its lines.
                    if file.text.is_none() {
                        file.diff.set_syntax(syntax.clone());
                    }
                    file.hunk_syntax = syntax;
                    file.highlighted = true;
                    cx.notify();
                }
                this.highlight_next(cx);
            })
            .ok();
        }));
    }

    fn stamp(&mut self) -> u64 {
        self.next_stamp += 1;
        self.next_stamp
    }

    /// Show the diff of `commit` (`None`: all commits), from the top.
    pub(super) fn select_commit(&mut self, commit: Option<String>, cx: &mut Context<Self>) {
        if commit == self.commit {
            return;
        }
        self.commit = commit;
        self.scope += 1;
        self.pages.clear();
        self.loading = None;
        self.highlighting = None;
        self.load_error = None;
        self.selection.clear();
        self.draft = None;
        self.rows.clear();
        self.list_state.reset(0);
        self.sync_files(cx);
        self.load_page(None, cx);
        cx.notify();
    }

    fn file(&self, shown: Shown) -> &LoadedFile {
        &self.pages[shown.page].files[shown.file]
    }

    fn file_mut(&mut self, shown: Shown) -> &mut LoadedFile {
        &mut self.pages[shown.page].files[shown.file]
    }

    /// The shown file at `path`, by its index in the list.
    fn shown_ix(&self, path: &str) -> Option<usize> {
        self.shown.iter().position(|&s| self.file(s).path == path)
    }

    /// Which files the list shows, in which order.
    fn sync_files(&mut self, cx: &mut Context<Self>) {
        let all: Vec<Shown> = self
            .pages
            .iter()
            .enumerate()
            .flat_map(|(page, p)| (0..p.files.len()).map(move |file| Shown { page, file }))
            .collect();
        let paths: Vec<&str> = all.iter().map(|&s| self.file(s).path.as_str()).collect();
        let ordered: Vec<Shown> = order_like_tree(&paths)
            .into_iter()
            .map(|ix| all[ix])
            .collect();
        self.test_files = ordered.iter().filter(|&&s| self.file(s).is_test).count();
        let hide_tests = self.code.hide_tests;
        self.shown = ordered
            .into_iter()
            .filter(|&s| !(hide_tests && self.file(s).is_test))
            .collect();
        let shown: Vec<String> = self
            .shown
            .iter()
            .map(|&s| self.file(s).path.clone())
            .collect();
        self.selection.retain(|path| shown.contains(path));
        if self
            .draft
            .as_ref()
            .is_some_and(|d| !shown.contains(&d.path))
        {
            self.draft = None;
        }
        self.place_annotations(cx);
        self.highlight_next(cx);
    }

    fn set_hide_tests(&mut self, hide: bool, cx: &mut Context<Self>) {
        self.change_code(cx, |code| code.hide_tests = hide);
        self.sync_files(cx);
        cx.notify();
    }

    // -- Diff options ------------------------------------------------------

    fn diff_view(&self) -> DiffView {
        DiffView {
            hide_whitespace: self.code.hide_whitespace,
            layout: self.code.layout,
        }
    }

    /// Change the Code tab's preferences, here and remembered.
    fn change_code(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut CodePreferences)) {
        change(&mut self.code);
        let code = self.code;
        preferences::update(cx, |preferences| preferences.code = code);
    }

    /// Every file gets new rows (their heights change).
    fn restamp(&mut self) {
        for page in 0..self.pages.len() {
            for file in 0..self.pages[page].files.len() {
                let stamp = self.stamp();
                self.pages[page].files[file].stamp = stamp;
            }
        }
        self.dirty = true;
    }

    fn set_layout(&mut self, layout: DiffLayout, cx: &mut Context<Self>) {
        if layout == self.code.layout {
            return;
        }
        self.change_code(cx, |code| code.layout = layout);
        for file in self.pages.iter_mut().flat_map(|p| &mut p.files) {
            let breaks = std::mem::take(&mut file.breaks);
            file.cut(&breaks, layout);
        }
        self.restamp();
        cx.notify();
    }

    fn set_wrap(&mut self, wrap: bool, cx: &mut Context<Self>) {
        self.change_code(cx, |code| code.wrap = wrap);
        self.restamp();
        cx.notify();
    }

    /// Like T3 Code, the selection and an open comment box go: their rows
    /// change.
    fn set_hide_whitespace(&mut self, hide: bool, cx: &mut Context<Self>) {
        self.change_code(cx, |code| code.hide_whitespace = hide);
        let view = self.diff_view();
        for file in self.pages.iter_mut().flat_map(|p| &mut p.files) {
            file.reshape(view);
        }
        self.restamp();
        self.selection.clear();
        self.draft = None;
        self.place_annotations(cx);
    }

    fn set_file_tree(&mut self, open: bool, cx: &mut Context<Self>) {
        self.change_code(cx, |code| code.file_tree = open);
        cx.notify();
    }

    // -- Context -----------------------------------------------------------

    /// A click on a gap of the shown file: reveal its lines, once the
    /// file's text is read.
    fn reveal(&mut self, shown: Shown, gap: usize, reveal: Reveal, cx: &mut Context<Self>) {
        let Some(file) = self
            .pages
            .get_mut(shown.page)
            .and_then(|p| p.files.get_mut(shown.file))
        else {
            return;
        };
        if file.text.is_none() {
            self.queued_reveals
                .entry(file.path.clone())
                .or_default()
                .push((gap, reveal));
            self.read_text(shown, cx);
            return;
        }
        file.revealed.entry(gap).or_default().apply(reveal);
        self.file_reshaped(shown, cx);
    }

    /// The shown file changed shape: new rows, annotations placed anew.
    fn file_reshaped(&mut self, shown: Shown, cx: &mut Context<Self>) {
        let view = self.diff_view();
        let stamp = self.stamp();
        let file = self.file_mut(shown);
        file.reshape(view);
        file.stamp = stamp;
        self.place_annotations(cx);
    }

    /// Read the shown file's text (both sides, parsed for highlighting),
    /// one read per file at a time; the clicks queued meanwhile apply then.
    fn read_text(&mut self, shown: Shown, cx: &mut Context<Self>) {
        let file = self.file(shown);
        let path = file.path.clone();
        if self.text_reads.contains_key(&path) {
            return;
        }
        let Some(service) = session_service(cx) else {
            return;
        };
        let change = match file.status {
            git::ChangeStatus::Renamed => FileChange::Renamed,
            _ => FileChange::Modified,
        };
        let old_path = file.orig_path.clone().unwrap_or_else(|| path.clone());
        let (pr, commit, scope) = (self.pr.clone(), self.commit.clone(), self.scope);
        let theme = cx.theme().highlight_theme.clone();
        let read = {
            let path = path.clone();
            cx.spawn(async move |this, cx| {
                let contents = service
                    .pull_request_file_contents(pr, commit, change, old_path, path.clone())
                    .await;
                let text = match contents {
                    Ok(contents) => {
                        let path = path.clone();
                        Ok(cx
                            .background_spawn(async move {
                                let syntax = DiffSyntax::parse(
                                    &path,
                                    Some(&contents.old),
                                    Some(&contents.new),
                                )
                                .map(|syntax| {
                                    syntax.prime(&theme);
                                    Arc::new(syntax)
                                });
                                FileText {
                                    new_lines: lines_of(&contents.new),
                                    syntax,
                                }
                            })
                            .await)
                    }
                    Err(error) => Err(error),
                };
                this.update(cx, |this, cx| this.text_read(&path, scope, text, cx))
                    .ok();
            })
        };
        self.text_reads.insert(path, read);
        cx.notify();
    }

    fn text_read(
        &mut self,
        path: &str,
        scope: u64,
        text: anyhow::Result<FileText>,
        cx: &mut Context<Self>,
    ) {
        self.text_reads.remove(path);
        let reveals = self.queued_reveals.remove(path).unwrap_or_default();
        let text = match text {
            Ok(text) => text,
            Err(error) => {
                // T3 Code only logs it; the next click tries again.
                self.data.update(cx, |data, cx| {
                    data.error = Some(format!("Could not read {path}: {error:#}").into());
                    cx.notify();
                });
                cx.notify();
                return;
            }
        };
        let Some(shown) = self
            .shown_ix(path)
            .map(|ix| self.shown[ix])
            .filter(|_| self.scope == scope)
        else {
            return;
        };
        let file = self.file_mut(shown);
        file.text = Some(text);
        for (gap, reveal) in reveals {
            file.revealed.entry(gap).or_default().apply(reveal);
        }
        self.file_reshaped(shown, cx);
    }

    // -- File tree ---------------------------------------------------------

    /// Show the shown file `ix` from its header (unfolded), as a click in
    /// the file tree asks.
    fn reveal_file(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(&shown) = self.shown.get(ix) else {
            return;
        };
        let path = self.file(shown).path.clone();
        if self.folds.is_folded(&path) {
            self.folds.set(&path, false);
            self.dirty = true;
        }
        self.sync_rows();
        if let Some(row) = self
            .rows
            .iter()
            .position(|row| *row == CodeRow::File { file: ix })
        {
            self.list_state.scroll_to(ListOffset {
                item_ix: row,
                offset_in_item: px(0.),
            });
        }
        self.tree_selected = Some(path);
        cx.notify();
    }

    fn toggle_folder(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.tree_collapsed.remove(&path) {
            self.tree_collapsed.insert(path);
        }
        cx.notify();
    }

    /// Collapse every folder, or expand them all when all are collapsed.
    fn toggle_all_folders(&mut self, cx: &mut Context<Self>) {
        let paths: Vec<String> = self
            .shown
            .iter()
            .map(|&s| self.file(s).path.clone())
            .collect();
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        let folders = tree::folder_paths(&paths);
        if !folders.is_empty() && folders.is_subset(&self.tree_collapsed) {
            self.tree_collapsed.clear();
        } else {
            self.tree_collapsed = folders;
        }
        cx.notify();
    }

    /// Put threads, pending comments and the comment box below their rows
    /// (in a commit's scope none: its line numbers are not the change's),
    /// and cut the chunks there.
    fn place_annotations(&mut self, cx: &mut Context<Self>) {
        // Held apart from `self`, which the loop changes.
        let activity = self.activity.clone();
        let threads = activity.as_ref().map_or(&[][..], |a| &a.review_threads[..]);
        let pending = self.drafts.read(cx).comments(&self.pr).to_vec();
        let whole_change = self.commit.is_none();
        let mut placed_threads = vec![false; threads.len()];
        let mut annotations = Vec::with_capacity(self.shown.len());
        for ix in 0..self.shown.len() {
            let shown = self.shown[ix];
            let file = self.file(shown);
            let mut items: Vec<(LineRef, Annotation)> = Vec::new();
            if whole_change {
                items.extend(threads.iter().enumerate().filter_map(|(ix, t)| {
                    let line = t
                        .line
                        .filter(|_| t.path == file.path && !t.comments.is_empty())?;
                    Some((LineRef { side: t.side, line }, Annotation::Thread(ix)))
                }));
                items.extend(pending.iter().filter(|c| c.path == file.path).map(|c| {
                    (
                        LineRef {
                            side: c.side,
                            line: c.line,
                        },
                        Annotation::Pending(c.id),
                    )
                }));
                if let Some(draft) = self.draft.as_ref().filter(|d| d.path == file.path) {
                    items.push((draft.at.end, Annotation::Draft));
                }
            }
            let (placed, _) = place(&file.lines, items);
            for annotation in placed.values().flatten() {
                if let Annotation::Thread(ix) = annotation {
                    placed_threads[*ix] = true;
                }
            }
            let breaks: Vec<usize> = placed.keys().copied().collect();
            if file.breaks != breaks {
                let (stamp, layout) = (self.stamp(), self.code.layout);
                let file = self.file_mut(shown);
                file.cut(&breaks, layout);
                file.stamp = stamp;
            }
            annotations.push(placed);
        }
        self.annotations = annotations;
        self.off_diff = (0..threads.len())
            .filter(|&ix| !placed_threads[ix] && !threads[ix].comments.is_empty())
            .collect();
        self.dirty = true;
        cx.notify();
    }

    // -- Viewed ------------------------------------------------------------

    fn read_viewed(&mut self, cx: &mut Context<Self>) {
        let Some(service) = session_service(cx) else {
            return;
        };
        let pr = self.pr.clone();
        self.viewed_read = Some(cx.spawn(async move |this, cx| {
            let result = service.pull_request_files_viewed(pr).await;
            this.update(cx, |this, cx| {
                this.viewed_read = None;
                match result {
                    Ok(fetched) => this.viewed.answer(&fetched.value, fetched.fetched_at),
                    Err(error) => tracing::debug!("files viewed not read: {error:#}"),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// The checkbox of the shown file `ix` was clicked; with `extend`
    /// (shift) every file from the last clicked one is ticked alike.
    /// Ticking folds, unticking unfolds.
    fn set_viewed(&mut self, ix: usize, viewed: bool, extend: bool, cx: &mut Context<Self>) {
        let Some(&shown) = self.shown.get(ix) else {
            return;
        };
        let anchor = extend
            .then(|| self.last_ticked.as_deref().and_then(|p| self.shown_ix(p)))
            .flatten();
        let range = viewed_range(ix, anchor);
        for &file in &self.shown[range] {
            let path = &self.pages[file.page].files[file.file].path;
            self.viewed.press(path, viewed);
            self.folds.set(path, viewed);
        }
        self.last_ticked = Some(self.file(shown).path.clone());
        self.viewed_flush = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(VIEWED_FLUSH_DELAY).await;
            this.update(cx, |this, cx| {
                this.viewed_flush = None;
                this.flush_viewed(cx);
            })
            .ok();
        }));
        self.dirty = true;
        cx.notify();
    }

    /// Write the queued presses, one batch at a time.
    fn flush_viewed(&mut self, cx: &mut Context<Self>) {
        let Some(batch) = self.viewed.take_batch() else {
            return;
        };
        let Some(service) = session_service(cx) else {
            self.viewed_written(&batch, false, cx);
            return;
        };
        let pr = self.pr.clone();
        self.viewed_write = Some(cx.spawn(async move |this, cx| {
            let ok = service
                .pull_request_set_files_viewed(pr, batch.files.clone())
                .await
                .inspect_err(|error| tracing::warn!("files viewed not written: {error:#}"))
                .is_ok();
            this.update(cx, |this, cx| this.viewed_written(&batch, ok, cx))
                .ok();
        }));
    }

    fn viewed_written(&mut self, batch: &Batch, ok: bool, cx: &mut Context<Self>) {
        self.viewed_write = None;
        let finished = self.viewed.finish(batch, ok);
        if finished.report_failure {
            self.data.update(cx, |data, cx| {
                data.error = Some("The viewed files could not be saved.".into());
                cx.notify();
            });
        }
        if finished.refresh {
            self.read_viewed(cx);
        }
        // Presses made while this batch was under way.
        if self.viewed.has_queued() && self.viewed_flush.is_none() {
            self.flush_viewed(cx);
        }
        cx.notify();
    }

    fn write_remaining_viewed(&mut self, cx: &mut App) {
        let files = self.viewed.take_remaining();
        if files.is_empty() {
            return;
        }
        let Some(service) = session_service(cx) else {
            return;
        };
        let pr = self.pr.clone();
        cx.spawn(async move |_| {
            if let Err(error) = service.pull_request_set_files_viewed(pr, files).await {
                tracing::warn!("files viewed not written: {error:#}");
            }
        })
        .detach();
    }

    // -- Folds -------------------------------------------------------------

    fn toggle_fold(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(&shown) = self.shown.get(ix) {
            let path = self.file(shown).path.clone();
            self.folds.toggle(&path);
            self.dirty = true;
            cx.notify();
        }
    }

    fn all_folded(&self) -> bool {
        self.folds
            .all_folded(self.shown.iter().map(|&s| self.file(s).path.as_str()))
    }

    fn fold_all(&mut self, folded: bool, cx: &mut Context<Self>) {
        self.folds.set_all(folded);
        self.dirty = true;
        cx.notify();
    }

    // -- List --------------------------------------------------------------

    /// Bring the rows and the list in step with the state: one splice.
    fn sync_rows(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let versions = &self.thread_versions;
        let thread_version = |ix: usize| versions.get(ix).copied().unwrap_or_default();
        let outlines: Vec<FileOutline> = self
            .shown
            .iter()
            .zip(&self.annotations)
            .map(|(&shown, annotations)| {
                let file = self.file(shown);
                let diff = &file.diff;
                let body = if diff.is_binary || diff.too_large || diff.hunks.is_empty() {
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
                    folded: self.folds.is_folded(&file.path),
                    stamp: file.stamp,
                    body,
                }
            })
            .collect();
        let footer = self.pages.last().is_some_and(|p| p.next_cursor.is_some());
        let rows = flatten(&outlines, footer);
        if let Some((old, count)) = changed_span(&self.rows, &rows) {
            self.list_state.splice(old, count);
        }
        if !rows.is_empty()
            && let Some(mounted) = self.mounted_at.take()
        {
            tracing::debug!(elapsed = ?mounted.elapsed(), "pull request code tab: first rows");
        }
        self.rows = rows;
    }

    /// The thread card for `thread`, made on its first showing.
    fn thread_card(&mut self, thread: usize, cx: &mut Context<Self>) -> Option<Entity<ThreadCard>> {
        let activity = self.activity.clone()?;
        let thread = activity.review_threads.get(thread)?;
        if let Some((card, _)) = self.thread_cards.get(&thread.id) {
            return Some(card.clone());
        }
        let card = ThreadCard::for_thread(self.data.clone(), thread)?;
        let card = cx.new(|_| card);
        let intents = cx.subscribe(&card, |_, _, intent: &PullRequestIntent, cx| {
            cx.emit(intent.clone())
        });
        self.thread_cards
            .insert(thread.id.clone(), (card.clone(), intents));
        Some(card)
    }
}

/// What a thread's row shows, for telling a changed thread apart.
fn thread_version(thread: &ReviewThread) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{thread:?}").hash(&mut hasher);
    hasher.finish()
}

impl SelectsLines for CodeTab {
    type Key = String;

    fn line_selection(&mut self) -> &mut LineSelection<String> {
        &mut self.selection
    }
}

impl Focusable for CodeTab {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CodeTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.started {
            self.start(cx);
        }
        self.sync_rows();
        let toolbar = self.render_toolbar(cx);
        let content = match self.render_state(cx) {
            Some(state) => state,
            None => {
                let list = list(
                    self.list_state.clone(),
                    cx.processor(|this: &mut Self, ix: usize, window, cx| {
                        this.render_row(ix, window, cx)
                    }),
                )
                .size_full();
                let code = div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(list)
                    .children(self.render_sticky_header(cx));
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(code)
                    .children(self.render_file_tree(window, cx))
                    .into_any_element()
            }
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let keys = &event.keystroke;
                if keys.modifiers.secondary() && keys.key == "c" {
                    this.copy_selection(cx);
                    cx.stop_propagation();
                } else if keys.modifiers.secondary() && keys.modifiers.shift && keys.key == "m" {
                    this.start_comment(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(toolbar)
            .children(self.render_off_diff(cx))
            .child(content)
            .children(self.render_floating(window, cx))
    }
}

#[cfg(test)]
mod tests;
