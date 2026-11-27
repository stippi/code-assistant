//! The Review tab: the local changes of one repository — the working tree,
//! the branch against a base, or what is not pushed yet — shown like a pull
//! request's Code tab, on the same [`code_view`](super::code_view): files
//! stacked under sticky headers, the same diff options, folds, file tree,
//! comment box and "Viewed" ticks, the latter kept locally ([`viewed`]).
//!
//! - **Repositories**: the session's folder may hold several; one shows at
//!   a time, chosen with the shared
//!   [`RepoPicker`](super::repo_picker::RepoPicker) ([`listing`]).
//! - **Listing**: the changed files of every repository arrive through the
//!   `current_review_listing` global, scanned repo by repo in the
//!   background; the view consumes it in `render` (a generation compare
//!   when nothing changed). While it has a listing it watches the
//!   repositories and asks for a new one on changes.
//! - **Diffs**: read one file at a time, the ones in view first, both sides
//!   at once, so unchanged lines reveal without another read ([`diffs`]).
//!   Each listed file carries a fingerprint; a diff read for an older one
//!   keeps showing until its replacement arrives.
//! - **Comments** go to the agent's draft; the draft's comments on the
//!   shown files sit below their lines ([`comments`]).

mod comments;
mod diffs;
mod listing;
mod viewed;

#[cfg(test)]
mod tests;

use super::code_view::anchors::{Annotation, LineRef};
use super::code_view::draft::{copy_selection, render_floating, start_comment};
use super::code_view::file::LoadedFile;
use super::code_view::file_order::{order_files, order_like_tree};
use super::code_view::list::{self as code_list, CodeHost, CodeList, ViewedMark};
use super::code_view::rows::CodeRow;
use super::code_view::{aside, body as code_body, options, preferences};
use super::line_selection::{LineSelection, SelectsLines};
use super::repo_picker::RepoPicker;
use crate::RepoReviewData;
use code_assistant_core::line_comments::LineComment;
use code_assistant_core::session::ReviewMode;
use git::ChangedFile;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::select::SelectState;
use gpui_kit::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, KeyDownEvent, Render, Subscription,
    Task, Window, div, list, prelude::*,
};
use listing::{BaseOption, ModeOption};
use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use viewed::{Tick, Ticks};

/// Sentinel for "never synced": guarantees the first generation compare
/// mismatches, whatever the global's current generation is.
const GENERATION_UNSEEN: u64 = u64::MAX;

/// One listed file of the shown repository.
struct ReviewFile {
    listed: ChangedFile,
    /// What the list shows: a header only until the diff is read.
    file: LoadedFile,
    /// The listing entry the diff was read for; another one means it is
    /// stale (and read again).
    read_for: Option<ChangedFile>,
    /// The content token of the diff read (see
    /// [`content_token`](crate::shared::reviewed_files_store::content_token)).
    content: Option<String>,
}

impl ReviewFile {
    fn current(&self) -> bool {
        self.read_for.as_ref() == Some(&self.listed)
    }
}

pub struct ReviewView {
    session_id: Option<String>,
    mode: ReviewMode,
    mode_state: Entity<SelectState<Vec<ModeOption>>>,
    base_state: Entity<SelectState<Vec<BaseOption>>>,
    is_git_repo: bool,
    /// Whether the first discovery response has arrived.
    has_listing: bool,

    /// Every repository of the session's folder, in listing order.
    repos: Vec<RepoReviewData>,
    /// The one shown, by its root.
    repo: Option<PathBuf>,
    picker: Entity<RepoPicker<PathBuf>>,
    picker_open: bool,
    /// The user's base choices for this session, per repository.
    base_overrides: HashMap<PathBuf, String>,
    /// Remembered default base, seeds a repo's base when it has no override.
    default_base: Option<String>,

    /// The shown repository's files, in listing order; `shown` indexes
    /// them in list order.
    files: Vec<ReviewFile>,
    shown: Vec<usize>,
    test_files: usize,
    /// The diff being read, by path.
    in_flight: Option<(String, Task<()>)>,
    /// Bumped whenever what the diffs are read against changes (repo,
    /// mode, base); arrivals for an older one are dropped.
    scope: u64,
    ticks: Ticks,

    /// Every comment of the draft; the shown files' ones sit below their
    /// lines.
    comments: Vec<LineComment>,
    /// A comment to show once its file's diff is read.
    pending_reveal: Option<LineComment>,

    /// Filesystem watcher on the listed repos (keyed by their roots so a
    /// changed set restarts it). Dropping it stops watching.
    watcher: Option<(Vec<PathBuf>, git::ChangeWatcher)>,
    watch_task: Option<Task<()>>,
    /// Generation of the consumed listing.
    listing_generation: u64,

    list: CodeList,
    _subscriptions: Vec<Subscription>,
}

impl ReviewView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (mode_state, base_state, picker, subscriptions) = listing::controls(window, cx);
        let default_base = cx
            .try_global::<crate::UiSettingsGlobal>()
            .and_then(|g| g.0.review_default_base.clone());
        Self {
            session_id: None,
            mode: ReviewMode::WorkingTree,
            mode_state,
            base_state,
            is_git_repo: false,
            has_listing: false,
            repos: Vec::new(),
            repo: None,
            picker,
            picker_open: false,
            base_overrides: HashMap::new(),
            default_base,
            files: Vec::new(),
            shown: Vec::new(),
            test_files: 0,
            in_flight: None,
            scope: 0,
            ticks: Ticks::default(),
            comments: Vec::new(),
            pending_reveal: None,
            watcher: None,
            watch_task: None,
            listing_generation: GENERATION_UNSEEN,
            list: CodeList::new("review", preferences::read(cx), cx.focus_handle()),
            _subscriptions: subscriptions,
        }
    }

    /// The shown repository's listing.
    fn repo_data(&self) -> Option<&RepoReviewData> {
        let root = self.repo.as_ref()?;
        self.repos.iter().find(|r| &r.repo_root == root)
    }

    /// The listed file at `path`.
    fn review_file(&self, path: &str) -> Option<&ReviewFile> {
        self.files.iter().find(|f| f.listed.path == path)
    }

    fn tick_of(&self, file: &ReviewFile) -> Tick {
        let content = file.current().then_some(file.content.as_deref()).flatten();
        self.ticks.tick(
            &file.listed.path,
            file.listed.fingerprint.as_deref(),
            content,
        )
    }

    /// Write the shown repository's ticks, off the UI thread.
    fn save_ticks(&self, cx: &mut Context<Self>) {
        let (Some(root), Some(store)) = (
            self.repo.clone(),
            cx.try_global::<crate::Gpui>()
                .map(|gpui| gpui.stores.reviewed_files.clone()),
        ) else {
            return;
        };
        let files = self.ticks.files().clone();
        cx.background_spawn(async move {
            if let Err(error) = store.save(&root, &files) {
                tracing::warn!("Review: viewed files not saved: {error:#}");
            }
        })
        .detach();
    }

    /// The toolbar above the list: repository, compare mode, base.
    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        div()
            .flex_none()
            .px_4()
            .py_2()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.muted_foreground)
            .children(self.render_repo_trigger(cx))
            .child(self.render_mode_select(cx))
            .child(self.render_base(cx))
            .into_any_element()
    }

    /// The code toolbar: counts and the diff options.
    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let left = div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .child(options::render_counts(self, false))
            .children(options::render_hidden_tests(self, cx))
            .into_any_element();
        let right = options::render_diff_options(self, cx);
        options::render_toolbar(self, left, right, cx)
    }

    /// Text in place of the list.
    fn note(text: &'static str, cx: &App) -> AnyElement {
        div()
            .px_4()
            .py_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(text)
            .into_any_element()
    }
}

impl CodeHost for ReviewView {
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
        &self.files[self.shown[ix]].file
    }

    fn shown_file_mut(&mut self, ix: usize) -> &mut LoadedFile {
        &mut self.files[self.shown[ix]].file
    }

    fn files_and_list(&mut self) -> (Vec<&mut LoadedFile>, &mut CodeList) {
        let files = self.files.iter_mut().map(|f| &mut f.file).collect();
        (files, &mut self.list)
    }

    /// Which files the list shows, in which order: like a file tree,
    /// tests left out when hidden.
    fn sync_files(&mut self, cx: &mut Context<Self>) {
        // By path, sources before tests before generated files, then like
        // a file tree; the diffs (and their imports) arrive too late to
        // order by without moving rows.
        let mut by_path: Vec<usize> = (0..self.files.len()).collect();
        by_path.sort_by(|&a, &b| self.files[a].listed.path.cmp(&self.files[b].listed.path));
        let paths: Vec<&str> = by_path
            .iter()
            .map(|&ix| self.files[ix].listed.path.as_str())
            .collect();
        let tiered: Vec<usize> = order_files(&paths, |_| std::iter::empty::<&str>())
            .into_iter()
            .map(|ix| by_path[ix])
            .collect();
        let paths: Vec<&str> = tiered
            .iter()
            .map(|&ix| self.files[ix].listed.path.as_str())
            .collect();
        let ordered: Vec<usize> = order_like_tree(&paths)
            .into_iter()
            .map(|ix| tiered[ix])
            .collect();
        self.test_files = self.files.iter().filter(|f| f.file.is_test).count();
        let hide_tests = self.list.code.hide_tests;
        self.shown = ordered
            .into_iter()
            .filter(|&ix| !(hide_tests && self.files[ix].file.is_test))
            .collect();
        let shown: Vec<String> = self
            .shown
            .iter()
            .map(|&ix| self.files[ix].listed.path.clone())
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
        self.ensure_diff_request(cx);
    }

    fn test_files(&self) -> usize {
        self.test_files
    }

    fn viewed(&self, path: &str) -> ViewedMark {
        self.review_file(path)
            .map_or(Tick::Unviewed, |file| self.tick_of(file))
            .mark()
    }

    /// Ticking folds, unticking unfolds; the ticks are saved at once.
    fn set_viewed(&mut self, ix: usize, viewed: bool, extend: bool, cx: &mut Context<Self>) {
        if ix >= self.shown.len() {
            return;
        }
        let path = self.shown_file(ix).path.clone();
        let paths: Vec<String> = self
            .shown
            .iter()
            .map(|&f| self.files[f].listed.path.clone())
            .collect();
        let range = self.list.ticked_range(ix, &path, extend, |p| {
            paths.iter().position(|other| other == p)
        });
        for &f in &self.shown[range] {
            let file = &self.files[f];
            let content = file.current().then_some(file.content.as_deref()).flatten();
            self.ticks.set(
                &file.listed.path,
                viewed,
                file.listed.fingerprint.as_deref(),
                content,
            );
            self.list.folds.set(&file.listed.path, viewed);
        }
        self.save_ticks(cx);
        self.list.dirty = true;
        self.ensure_diff_request(cx);
        cx.notify();
    }

    /// The draft's comments on the shown files below their lines.
    fn place_annotations(&mut self, cx: &mut Context<Self>) {
        let root = self.repo.clone().unwrap_or_default();
        let items: Vec<Vec<(LineRef, Annotation)>> = (0..self.shown.len())
            .map(|ix| {
                let file = root.join(&self.shown_file(ix).path);
                self.comments
                    .iter()
                    .filter(|c| c.in_diff && c.file == file)
                    .map(|c| (comments::line_ref(c), Annotation::Pending(c.id)))
                    .collect()
            })
            .collect();
        code_list::place_annotations(self, items, cx);
    }

    fn comment_file(&self, path: &str) -> PathBuf {
        self.repo.clone().unwrap_or_default().join(path)
    }

    fn comment_marks(
        &self,
        file: &LoadedFile,
        lines: Range<usize>,
        _cx: &App,
    ) -> Vec<Range<usize>> {
        self.comment_rows(file, lines)
    }

    fn render_header_stat(&self, ix: usize, cx: &App) -> Option<AnyElement> {
        let file = self.shown_file(ix);
        if !file.loaded {
            let reading = self
                .in_flight
                .as_ref()
                .is_some_and(|(path, _)| *path == file.path);
            return Some(listing::scan_marker(reading, cx));
        }
        if file.diff.is_binary || file.diff.too_large {
            return None;
        }
        crate::main_screen::right_panel::pull_request::presentation::diff_stat(
            file.additions,
            file.deletions,
            cx,
        )
    }

    fn render_annotation(&mut self, row: &CodeRow, cx: &mut Context<Self>) -> AnyElement {
        match *row {
            CodeRow::Pending { id, .. } => self.render_comment(id, cx),
            _ => div().into_any_element(),
        }
    }
}

impl SelectsLines for ReviewView {
    type Key = String;

    fn line_selection(&mut self) -> &mut LineSelection<String> {
        &mut self.list.selection
    }
}

impl Focusable for ReviewView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.list.focus_handle.clone()
    }
}

impl Render for ReviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Pull a fresh listing before laying out: a generation compare when
        // nothing changed.
        self.sync_listing(window, cx);
        self.apply_pending_reveal(window, cx);
        code_list::sync_rows(self);

        let muted = cx.theme().muted_foreground;
        // Before the first (fast) discovery response there is nothing to
        // lay out yet.
        if !self.has_listing {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .p_4()
                .text_sm()
                .text_color(muted)
                .child(listing::scan_marker(true, cx))
                .child("Looking for repositories…")
                .into_any_element();
        }
        if !self.is_git_repo {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_sm()
                .text_color(muted)
                .child("Not a git repository")
                .into_any_element();
        }
        if self.repo.is_none() {
            return self.picker.clone().into_any_element();
        }

        let content = match self.repo_data() {
            Some(data) if data.files.is_empty() => Self::note(
                match data.scan_state {
                    code_assistant_core::session::ReviewScanState::Done => "No changes.",
                    _ => "Looking for changes…",
                },
                cx,
            ),
            _ => {
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
            .child(self.render_header(cx))
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().flex().flex_col().child(content))
            .children(render_floating(self, window, cx))
            .into_any_element()
    }
}
