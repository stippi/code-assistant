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
//!   list them ([`file_order`]); tests can be hidden ([`file_kind`]).
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
mod body;
mod comments;
mod file_header;
mod file_kind;
mod file_order;
mod folds;
mod pages;
mod rows;
mod toolbar;
mod viewed;

use super::PullRequestIntent;
use super::bus::{PullRequestEvent, bus};
use super::conversation::thread::ThreadCard;
use super::data::{PullRequestData, session_service};
use super::review_draft::{ReviewDrafts, review_drafts};
use crate::comments::AnchorTracker;
use crate::main_screen::right_panel::line_selection::{LineSelection, Selected, SelectsLines};
use crate::main_screen::right_panel::review_rows::changed_span;
use anchors::{Annotation, LineRef, place};
use code_assistant_core::pull_requests::PullRequestPart;
use comments::Draft;
use file_order::order_like_tree;
use folds::{Folds, viewed_range};
use github::PullRequestRef;
use github::model::{Activity, ReviewThread};
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    ListAlignment, ListState, Pixels, Render, SharedString, Subscription, Task, Window, div, list,
    prelude::*, px,
};
use pages::{Arrival, LoadedFile, Page, arrival, fingerprint, parse_page};
use rows::{CodeRow, FileBody, FileOutline, flatten};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use viewed::{Batch, ViewedFiles};

/// Presses on "Viewed" wait this long for more before they are written.
const VIEWED_FLUSH_DELAY: Duration = Duration::from_millis(400);

/// How far beyond the viewport the list builds rows; the footer within it
/// asks for the next page.
const LIST_OVERDRAW: Pixels = px(512.);

/// A shown file: its page and its index there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shown {
    page: usize,
    file: usize,
}

pub struct CodeTab {
    data: Entity<PullRequestData>,
    pr: PullRequestRef,
    /// The commit the diff is scoped to; `None` for all of them.
    commit: Option<String>,
    /// Bumped with each new scope; arrivals for an older one are dropped.
    scope: u64,
    /// The first page was asked for (on the first draw).
    started: bool,
    pages: Vec<Page>,
    /// The page read under way.
    loading: Option<Task<()>>,
    load_error: Option<SharedString>,
    next_stamp: u64,

    /// The files in the list, in order.
    shown: Vec<Shown>,
    test_files: usize,
    hide_tests: bool,
    folds: Folds,

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
            load_error: None,
            next_stamp: 0,
            shown: Vec::new(),
            test_files: 0,
            hide_tests: false,
            folds: Folds::default(),
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
        // Captured here: the highlighting is computed off the UI thread.
        let theme = cx.theme().highlight_theme.clone();
        self.loading = Some(cx.spawn(async move |this, cx| {
            let result = service.pull_request_diff(pr, commit, cursor.clone()).await;
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
            let files = cx
                .background_spawn(async move { parse_page(&page, &theme) })
                .await;
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
            for file in &mut page.files {
                file.stamp = self.stamp();
            }
            self.pages.truncate(ix);
            self.pages.push(page);
            self.sync_files(cx);
        }
        cx.notify();
    }

    fn stamp(&mut self) -> u64 {
        self.next_stamp += 1;
        self.next_stamp
    }

    /// Show the diff of `commit` (`None`: all commits), from the top.
    fn select_commit(&mut self, commit: Option<String>, cx: &mut Context<Self>) {
        if commit == self.commit {
            return;
        }
        self.commit = commit;
        self.scope += 1;
        self.pages.clear();
        self.loading = None;
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
        self.shown = ordered
            .into_iter()
            .filter(|&s| !(self.hide_tests && self.file(s).is_test))
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
    }

    fn set_hide_tests(&mut self, hide: bool, cx: &mut Context<Self>) {
        self.hide_tests = hide;
        self.sync_files(cx);
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
                let stamp = self.stamp();
                let file = self.file_mut(shown);
                file.diff.rechunk(&breaks);
                file.breaks = breaks;
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
                    FileBody::Diff {
                        hunks: &diff.hunks,
                        chunks: &diff.chunked.chunks,
                        annotations,
                        thread_version: &thread_version,
                        tail: matches!(
                            file.status,
                            git::ChangeStatus::Modified | git::ChangeStatus::Renamed
                        ),
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
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(list)
                    .children(self.render_sticky_header(cx))
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
