//! The Code tab: a pull request's changed files (or one commit's) with
//! their diffs, review threads next to their lines and the pending
//! review's comments, modelled on T3 Code's `PullRequestCodeTab`.
//!
//! - **Loading**: the diff page by page (`pull_request_diff`), the first
//!   when the tab is first drawn, the next when the end of the list comes
//!   into view; each page parsed and highlighted once, off the UI thread
//!   ([`pages`]). A re-read (the core announces a changed diff, or the head
//!   moved) keeps unchanged pages and everything built from them.
//! - **Viewed**: optimistic ticks, written in batches ([`viewed`]).
//! - **Threads**: review threads and pending comments below their lines,
//!   threads no row shows in a strip above the list ([`body`]); pending
//!   comments in [`review_draft`](super::review_draft) ([`comments`]).
//! - **The rest** is the code views' ([`code_view`](crate::main_screen::right_panel::code_view)): order, hidden
//!   tests, file tree, stacked or split, wrap, whitespace, revealed lines,
//!   folds, the rows and the comment box. The tab is their [`CodeHost`];
//!   comments go to the pending review or to the agent's draft, not in a
//!   commit's scope (GitHub anchors comments to the whole change).

mod body;
mod comments;
mod pages;
mod toolbar;
mod viewed;

use super::PullRequestIntent;
use super::bus::{PullRequestEvent, bus};
use super::conversation::thread::ThreadCard;
use super::data::{PullRequestData, session_service};
use super::detail::TabScroll;
use super::review_draft::{ReviewDrafts, review_drafts};
use crate::diff::prepare::highlight_patch;
use crate::diff::syntax::DiffSyntax;
use crate::main_screen::right_panel::code_view::anchors::{Annotation, LineRef};
use crate::main_screen::right_panel::code_view::context::lines_of;
use crate::main_screen::right_panel::code_view::draft::{copy_selection, start_comment};
use crate::main_screen::right_panel::code_view::file::{FileText, LoadedFile};
use crate::main_screen::right_panel::code_view::file_order::order_like_tree;
use crate::main_screen::right_panel::code_view::list::{
    self as code_list, CodeHost, CodeList, ViewedMark,
};
use crate::main_screen::right_panel::code_view::rows::{CodeRow, files_by_proximity};
use crate::main_screen::right_panel::code_view::{aside, body as code_body, preferences};
use crate::main_screen::right_panel::line_selection::{LineSelection, SelectsLines};
use code_assistant_core::pull_requests::PullRequestPart;
use github::PullRequestRef;
use github::model::{Activity, FileChange, ReviewThread};
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent, Render,
    SharedString, Subscription, Task, Window, div, list, prelude::*,
};
use pages::{Arrival, Page, arrival, fingerprint, parse_page};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};
use viewed::{Batch, ViewedFiles};

/// Presses on "Viewed" wait this long for more before they are written.
const VIEWED_FLUSH_DELAY: Duration = Duration::from_millis(400);

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

    /// The files in the list, in order.
    shown: Vec<Shown>,
    test_files: usize,
    /// Files' texts being read, by path: one read per file at a time.
    text_reads: HashMap<String, Task<()>>,
    viewed: ViewedFiles,
    viewed_read: Option<Task<()>>,
    viewed_flush: Option<Task<()>>,
    viewed_write: Option<Task<()>>,

    activity: Option<Arc<Activity>>,
    head_sha: Option<String>,
    /// Per review thread: what its row shows, for the list's change check.
    thread_versions: Vec<u64>,
    /// Threads no rendered row shows, by index.
    off_diff: Vec<usize>,
    off_diff_open: bool,
    thread_cards: HashMap<String, (Entity<ThreadCard>, Subscription)>,
    /// How many commits the scope menu lists.
    commits_shown: usize,

    drafts: Entity<ReviewDrafts>,

    list: CodeList,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PullRequestIntent> for CodeTab {}

impl CodeTab {
    /// How far the tab is scrolled; the header condenses once it is.
    pub fn scroll(&self) -> TabScroll {
        TabScroll::of(&self.list.list_state)
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
            shown: Vec::new(),
            test_files: 0,
            text_reads: HashMap::new(),
            viewed: ViewedFiles::default(),
            viewed_read: None,
            viewed_flush: None,
            viewed_write: None,
            activity: None,
            head_sha: None,
            thread_versions: Vec::new(),
            off_diff: Vec::new(),
            off_diff_open: false,
            thread_cards: HashMap::new(),
            commits_shown: toolbar::COMMIT_PAGE,
            drafts,
            list: CodeList::new("pr-code", preferences::read(cx), cx.focus_handle()),
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
        let view = self.list.diff_view();
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
            let view = self.list.diff_view();
            for file in &mut page.files {
                file.reshape(view);
                file.stamp = self.list.stamp();
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
        code_list::sync_rows(self);
        let top = self.list.list_state.logical_scroll_top().item_ix;
        // A file whose text was read is highlighted from it.
        let Some(shown) = files_by_proximity(&self.list.rows, self.shown.len(), top)
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
        self.list.reset();
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

    // -- Context -----------------------------------------------------------

    fn text_read(
        &mut self,
        path: &str,
        scope: u64,
        text: anyhow::Result<FileText>,
        cx: &mut Context<Self>,
    ) {
        self.text_reads.remove(path);
        let text = match text {
            Ok(text) => text,
            Err(error) => {
                // T3 Code only logs it; the next click tries again.
                if let Some(ix) = self.shown_ix(path) {
                    self.shown_file_mut(ix).queued_reveals.clear();
                }
                self.data.update(cx, |data, cx| {
                    data.error = Some(format!("Could not read {path}: {error:#}").into());
                    cx.notify();
                });
                cx.notify();
                return;
            }
        };
        let Some(ix) = self.shown_ix(path).filter(|_| self.scope == scope) else {
            return;
        };
        code_list::text_read(self, ix, text, cx);
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

impl CodeHost for CodeTab {
    fn list(&self) -> &CodeList {
        &self.list
    }

    fn list_mut(&mut self) -> &mut CodeList {
        &mut self.list
    }

    fn shown_count(&self) -> usize {
        self.shown.len()
    }

    fn shown_file(&self, ix: usize) -> &LoadedFile {
        self.file(self.shown[ix])
    }

    fn shown_file_mut(&mut self, ix: usize) -> &mut LoadedFile {
        self.file_mut(self.shown[ix])
    }

    fn files_and_list(&mut self) -> (Vec<&mut LoadedFile>, &mut CodeList) {
        let files = self.pages.iter_mut().flat_map(|p| &mut p.files).collect();
        (files, &mut self.list)
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
        let hide_tests = self.list.code.hide_tests;
        self.shown = ordered
            .into_iter()
            .filter(|&s| !(hide_tests && self.file(s).is_test))
            .collect();
        let shown: Vec<String> = self
            .shown
            .iter()
            .map(|&s| self.file(s).path.clone())
            .collect();
        self.list.selection.retain(|path| shown.contains(path));
        if self
            .list
            .draft
            .as_ref()
            .is_some_and(|d| !shown.contains(&d.path))
        {
            self.list.draft = None;
        }
        self.place_annotations(cx);
        self.highlight_next(cx);
    }

    fn test_files(&self) -> usize {
        self.test_files
    }

    fn viewed(&self, path: &str) -> ViewedMark {
        ViewedMark {
            viewed: self.viewed.is_viewed(path),
            stale: self.viewed.is_stale(path),
        }
    }

    /// Ticking folds, unticking unfolds; the presses are written in a
    /// batch a moment later.
    fn set_viewed(&mut self, ix: usize, viewed: bool, extend: bool, cx: &mut Context<Self>) {
        let Some(&shown) = self.shown.get(ix) else {
            return;
        };
        let path = self.file(shown).path.clone();
        let range = {
            let paths: Vec<String> = self
                .shown
                .iter()
                .map(|&s| self.file(s).path.clone())
                .collect();
            self.list.ticked_range(ix, &path, extend, |p| {
                paths.iter().position(|other| other == p)
            })
        };
        for &file in &self.shown[range] {
            let path = &self.pages[file.page].files[file.file].path;
            self.viewed.press(path, viewed);
            self.list.folds.set(path, viewed);
        }
        self.viewed_flush = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(VIEWED_FLUSH_DELAY).await;
            this.update(cx, |this, cx| {
                this.viewed_flush = None;
                this.flush_viewed(cx);
            })
            .ok();
        }));
        self.list.dirty = true;
        cx.notify();
    }

    /// Put threads, pending comments and the comment box below their rows
    /// (in a commit's scope none: its line numbers are not the change's),
    /// and cut the chunks there.
    fn place_annotations(&mut self, cx: &mut Context<Self>) {
        let activity = self.activity.clone();
        let threads = activity.as_ref().map_or(&[][..], |a| &a.review_threads[..]);
        let pending = self.drafts.read(cx).comments(&self.pr).to_vec();
        let whole_change = self.commit.is_none();
        let items: Vec<Vec<(LineRef, Annotation)>> = (0..self.shown.len())
            .map(|ix| {
                let file = self.shown_file(ix);
                let mut items = Vec::new();
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
                }
                items
            })
            .collect();
        let mut placed_threads = vec![false; threads.len()];
        code_list::place_annotations(self, items, cx);
        for placed in self
            .list
            .annotations
            .iter()
            .flat_map(|a| a.values().flatten())
        {
            if let Annotation::Thread(ix) = placed {
                placed_threads[*ix] = true;
            }
        }
        self.off_diff = (0..threads.len())
            .filter(|&ix| !placed_threads[ix] && !threads[ix].comments.is_empty())
            .collect();
    }

    /// Read the shown file's text (both sides, parsed for highlighting),
    /// one read per file at a time; the clicks queued meanwhile apply then.
    fn read_text(&mut self, ix: usize, cx: &mut Context<Self>) {
        let file = self.shown_file(ix);
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

    fn text_loading(&self, path: &str) -> bool {
        self.text_reads.contains_key(path)
    }

    /// Not in a commit's scope: GitHub anchors comments to the whole change.
    fn can_comment(&self) -> bool {
        self.commit.is_none()
    }

    fn comment_host_target(&self) -> Option<&'static str> {
        Some("Add to review")
    }

    fn add_comment_to_host(&mut self, cx: &mut Context<Self>) {
        self.add_to_review(cx);
    }

    fn comment_marks(&self, file: &LoadedFile, lines: Range<usize>, cx: &App) -> Vec<Range<usize>> {
        self.pending_marks(file, lines, cx)
    }

    fn note_text(&self, file: &LoadedFile) -> &'static str {
        if file.diff.is_binary {
            "Binary file not shown."
        } else if file.diff.too_large || file.additions + file.deletions > 0 {
            "GitHub did not send this file's changes (too large)."
        } else if file.orig_path.is_some() {
            "Renamed without changes."
        } else {
            "No content changes."
        }
    }

    fn thread_version(&self, thread: usize) -> u64 {
        self.thread_versions
            .get(thread)
            .copied()
            .unwrap_or_default()
    }

    fn render_annotation(&mut self, row: &CodeRow, cx: &mut Context<Self>) -> AnyElement {
        match *row {
            CodeRow::Thread { thread, .. } => match self.thread_card(thread, cx) {
                Some(card) => crate::main_screen::right_panel::code_view::draft::annotation(
                    card.into_any_element(),
                ),
                None => div().into_any_element(),
            },
            CodeRow::Pending { id, .. } => self.render_pending(id, cx),
            _ => div().into_any_element(),
        }
    }

    fn has_footer(&self) -> bool {
        self.pages.last().is_some_and(|p| p.next_cursor.is_some())
    }

    fn render_footer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.render_list_footer(cx)
    }

    fn render_tree_footer(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.render_more_files(cx)
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
        &mut self.list.selection
    }
}

impl Focusable for CodeTab {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.list.focus_handle.clone()
    }
}

impl Render for CodeTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.started {
            self.start(cx);
        }
        code_list::sync_rows(self);
        if !self.list.rows.is_empty()
            && let Some(mounted) = self.mounted_at.take()
        {
            tracing::debug!(elapsed = ?mounted.elapsed(), "pull request code tab: first rows");
        }
        let toolbar = self.render_toolbar(cx);
        let content = match self.render_state(cx) {
            Some(state) => state,
            None => {
                let list = list(
                    self.list.list_state.clone(),
                    cx.processor(|this: &mut Self, ix: usize, window, cx| {
                        code_body::render_row(this, ix, window, cx)
                    }),
                )
                .size_full();
                let code = div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(list)
                    .children(code_body::render_sticky_header(self, cx));
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(code)
                    .children(aside::render_file_tree(self, window, cx))
                    .into_any_element()
            }
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .track_focus(&self.list.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let keys = &event.keystroke;
                if keys.modifiers.secondary(cx) && keys.key == "c" {
                    copy_selection(this, cx);
                    cx.stop_propagation();
                } else if keys.modifiers.secondary(cx) && keys.modifiers.shift && keys.key == "m" {
                    start_comment(this, window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(toolbar)
            .children(self.render_off_diff(cx))
            .child(content)
            .children(
                crate::main_screen::right_panel::code_view::draft::render_floating(
                    self, window, cx,
                ),
            )
    }
}

#[cfg(test)]
mod tests;
