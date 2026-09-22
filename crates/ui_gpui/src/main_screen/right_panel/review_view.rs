//! The "Review" view for the right sidebar: a compare-mode selector above a
//! single scrollable column of per-repo sections. Within a repo, changed files
//! are **stacked**: each file has a collapsible header (icon, path, status,
//! per-file `+/−`) with its diff hunks directly below — no separate tree/diff
//! split.
//!
//! Backend data arrives through the `current_review_listing` / `current_review_diff`
//! globals on [`Gpui`]; this view consumes them in `render` (the "sync-in-render"
//! technique the worktree selector uses). Change detection is generation-based:
//! the per-frame unchanged case costs an integer compare.
//!
//! The column is a virtualized `list()` over flat rows (see
//! [`super::review_rows`]): headers and diff chunks are separate items, so a
//! frame only builds what is in view. State changes reach the list as one
//! splice, which keeps measured heights and the scroll position.
//!
//! Diffs load lazily, one file at a time: after each arrival the next visible
//! file without a diff is requested. Hunks (changed lines + a few context
//! lines) are computed once on arrival and cached — rendering never diffs, and
//! the element count scales with changed lines, not file sizes.
//!
//! Freshness: while the view has a listing it owns a [`git::ChangeWatcher`]
//! on the listed repos and re-requests the listing whenever the watcher
//! reports activity (the main screen also reloads on window activation as a
//! safety net). Each changed file carries a fingerprint; a cached diff whose
//! listing entry changed is stale and re-requested, but keeps rendering until
//! its replacement arrives, so nothing flickers.

use super::review_rows::{
    DiffBody, FileOutline, RepoOutline, ReviewRow, changed_span, files_by_proximity, flatten,
};
use crate::shared::file_icons;
use crate::tool_cards::diff_card::{added_row_colors, deleted_row_colors, render_diff_chunk};
use crate::{Gpui, PreparedReviewDiff, RepoReviewData};
use code_assistant_core::session::{ReviewMode, ReviewScanState};
use git::{ChangeStatus, ChangedFile};
use gpui_kit::component::{
    ActiveTheme, Icon, Sizable, Size,
    scroll::ScrollableElement,
    select::{Select, SelectEvent, SelectItem, SelectState},
    v_flex,
};
use gpui_kit::{
    AnimationExt, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, ListAlignment,
    ListState, Render, Subscription, Task, Window, div, list, prelude::*, px, rems,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Compare-mode dropdown
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct ModeOption {
    label: String,
    value: ReviewMode,
}

impl SelectItem for ModeOption {
    type Value = ReviewMode;
    fn title(&self) -> gpui_kit::SharedString {
        self.label.clone().into()
    }
    fn display_title(&self) -> Option<gpui_kit::AnyElement> {
        None
    }
    fn value(&self) -> &Self::Value {
        &self.value
    }
}

fn mode_options() -> Vec<ModeOption> {
    vec![
        ModeOption {
            label: "Working tree".to_string(),
            value: ReviewMode::WorkingTree,
        },
        ModeOption {
            label: "Branch vs base".to_string(),
            value: ReviewMode::BranchVsBase,
        },
    ]
}

// ---------------------------------------------------------------------------
// Base-branch dropdown
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct BaseOption {
    branch: String,
}

impl SelectItem for BaseOption {
    type Value = String;
    fn title(&self) -> gpui_kit::SharedString {
        self.branch.clone().into()
    }
    fn display_title(&self) -> Option<gpui_kit::AnyElement> {
        None
    }
    fn value(&self) -> &Self::Value {
        &self.branch
    }
}

// ---------------------------------------------------------------------------
// Per-repo section
// ---------------------------------------------------------------------------

/// One git repo's stacked file list plus its base selector, rendered as a
/// collapsible section.
struct RepoSection {
    repo_root: PathBuf,
    label: String,
    base_state: Entity<SelectState<Vec<BaseOption>>>,
    base_candidates: Vec<String>,
    base: Option<String>,
    files: Vec<ChangedFile>,
    stats: git::DiffStats,
    scan_state: ReviewScanState,
    collapsed: bool,
    _base_sub: Subscription,
}

/// Single-letter status badge (same colors the old file tree used).
fn status_badge(status: ChangeStatus) -> (&'static str, gpui_kit::Hsla) {
    match status {
        ChangeStatus::Added | ChangeStatus::Untracked => ("A", gpui_kit::rgb(0x3f_a5_5a).into()),
        ChangeStatus::Modified => ("M", gpui_kit::rgb(0xc7_9a_3a).into()),
        ChangeStatus::Deleted => ("D", gpui_kit::rgb(0xc7_4a_4a).into()),
        ChangeStatus::Renamed => ("R", gpui_kit::rgb(0x4a_82_c7).into()),
        ChangeStatus::Copied => ("C", gpui_kit::rgb(0x4a_82_c7).into()),
        ChangeStatus::TypeChanged => ("T", gpui_kit::rgb(0x8a_6a_c7).into()),
    }
}

// ---------------------------------------------------------------------------
// ReviewView
// ---------------------------------------------------------------------------

/// Identifies one changed file across repos.
type FileKey = (PathBuf, String);

/// Sentinel for "never synced": guarantees the first generation compare
/// mismatches, whatever the global's current generation is.
const GENERATION_UNSEEN: u64 = u64::MAX;

/// Quiet period the change watcher waits before reporting a burst of
/// filesystem events, so an edit (or a build) costs one scan.
const REVIEW_WATCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(300);

/// How far beyond the viewport the list builds rows, so scrolling reveals
/// finished content.
const REVIEW_LIST_OVERDRAW: gpui_kit::Pixels = px(512.);

/// A prepared diff together with the listing entry it was loaded for. When a
/// later listing carries a different entry for the same path (new
/// fingerprint or status), the diff is stale.
struct LoadedDiff {
    file: ChangedFile,
    prepared: PreparedReviewDiff,
    /// Unique per arrival; tells the list rows of a reloaded diff apart.
    stamp: u64,
}

pub struct ReviewView {
    session_id: Option<String>,
    mode_state: Entity<SelectState<Vec<ModeOption>>>,

    /// Current compare mode (drives requests). Base is tracked per repo.
    mode: ReviewMode,
    is_git_repo: bool,
    /// Whether the first discovery response has arrived.
    has_listing: bool,

    /// Per-repo sections, in listing order.
    repos: Vec<RepoSection>,
    /// User's explicit per-repo base choices for this session.
    base_overrides: HashMap<PathBuf, String>,
    /// Persisted default base ref, seeds a repo's base when it has no override.
    default_base: Option<String>,

    /// Prepared diffs by file, filled lazily one request at a time. A stale
    /// entry (see [`LoadedDiff`]) stays here — and on screen — until its
    /// replacement arrives.
    file_diffs: HashMap<FileKey, LoadedDiff>,
    /// Files the user collapsed (default is expanded).
    collapsed_files: HashSet<FileKey>,
    /// The single outstanding diff request, with the listing entry it was
    /// made for; arrivals for anything else are stale (e.g. from before a
    /// mode/base change) and dropped.
    in_flight: Option<(FileKey, ChangedFile)>,
    next_diff_stamp: u64,

    /// The rows the list currently shows, and the list's state. `sync_rows`
    /// keeps both in step with the fields above.
    rows: Vec<ReviewRow>,
    list_state: ListState,

    /// Filesystem watcher on the listed repos (keyed by their roots so a
    /// changed set restarts it). Dropping it stops watching.
    watcher: Option<(Vec<PathBuf>, git::ChangeWatcher)>,
    /// Forwards watcher callbacks to `request_listing` on the UI thread.
    watch_task: Option<Task<()>>,

    /// Generation of the consumed listing. Change detection per frame is a
    /// plain integer compare against the global's generation — no clones.
    listing_generation: u64,
    /// Generation of the last consumed diff (see `listing_generation`).
    diff_generation: u64,

    focus_handle: FocusHandle,
    _mode_sub: Subscription,
}

impl EventEmitter<()> for ReviewView {}

impl ReviewView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mode_state = cx.new(|cx| {
            let mut state = SelectState::new(mode_options(), None, window, cx);
            state.set_selected_value(&ReviewMode::WorkingTree, window, cx);
            state
        });
        let mode_sub = cx.subscribe_in(&mode_state, window, Self::on_mode_event);

        // Seed the persisted default base from settings.
        let default_base = cx
            .try_global::<crate::UiSettingsGlobal>()
            .and_then(|g| g.0.review_default_base.clone());

        Self {
            session_id: None,
            mode_state,
            mode: ReviewMode::WorkingTree,
            is_git_repo: false,
            has_listing: false,
            repos: Vec::new(),
            base_overrides: HashMap::new(),
            default_base,
            file_diffs: HashMap::new(),
            collapsed_files: HashSet::new(),
            in_flight: None,
            next_diff_stamp: 0,
            rows: Vec::new(),
            list_state: ListState::new(0, ListAlignment::Top, REVIEW_LIST_OVERDRAW).measure_all(),
            watcher: None,
            watch_task: None,
            listing_generation: GENERATION_UNSEEN,
            diff_generation: GENERATION_UNSEEN,
            focus_handle: cx.focus_handle(),
            _mode_sub: mode_sub,
        }
    }

    /// Point the view at a session and request its changed files.
    pub fn set_session(&mut self, session_id: Option<String>, cx: &mut Context<Self>) {
        self.session_id = session_id;
        // Reset per-session state; fresh data will arrive via the global.
        self.has_listing = false;
        self.listing_generation = GENERATION_UNSEEN;
        self.diff_generation = GENERATION_UNSEEN;
        self.repos.clear();
        self.base_overrides.clear();
        self.file_diffs.clear();
        self.collapsed_files.clear();
        self.in_flight = None;
        // Start the new session scrolled to the top.
        self.rows.clear();
        self.list_state.reset(0);
        // A new session lists its own repos; the watcher follows the listing.
        self.watcher = None;
        self.watch_task = None;

        // Restore the persisted compare mode for this session. The selector
        // resyncs from the echoed listing on the next render.
        if let Some(id) = &self.session_id {
            if let Some(store) = crate::shared::ui_state::UiStateStore::try_global()
                && let Ok(mut store) = store.lock()
            {
                let mode = store.get_review_compare_mode(id);
                self.mode = match mode.as_deref() {
                    Some("branch_vs_base") => ReviewMode::BranchVsBase,
                    _ => ReviewMode::WorkingTree,
                };
            }
        } else {
            self.mode = ReviewMode::WorkingTree;
        }

        self.request_listing(cx);
    }

    /// Persist the current compare mode for the active session.
    fn persist_mode(&self, cx: &mut Context<Self>) {
        let Some(session_id) = &self.session_id else {
            return;
        };
        let mode = match self.mode {
            ReviewMode::WorkingTree => "working_tree",
            ReviewMode::BranchVsBase => "branch_vs_base",
        };
        if let Ok(mut store) = crate::shared::ui_state::UiStateStore::global().lock() {
            store.set_review_compare_mode(session_id, mode.to_string());
        }
        if let Some(sender) = cx.try_global::<crate::UiEventSender>() {
            let _ = sender
                .0
                .try_send(code_assistant_core::ui::ui_events::UiEvent::PersistUiState);
        }
    }

    /// Re-request the changed-files listing for the current mode.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.request_listing(cx);
    }

    /// Keep a change watcher running on exactly the listed repos. Watcher
    /// callbacks (background thread) are forwarded through a one-slot channel
    /// to `request_listing` on the UI thread; the listing's fingerprints then
    /// decide which diffs are stale.
    fn ensure_watcher(&mut self, cx: &mut Context<Self>) {
        let roots: Vec<PathBuf> = self.repos.iter().map(|r| r.repo_root.clone()).collect();
        if roots.is_empty() {
            self.watcher = None;
            self.watch_task = None;
            return;
        }
        if self.watcher.as_ref().is_some_and(|(r, _)| *r == roots) {
            return;
        }

        // A bounded(1) channel coalesces callbacks that land while the UI
        // thread is still busy; dropping the watcher closes it, ending the task.
        let (tx, rx) = async_channel::bounded::<()>(1);
        match git::ChangeWatcher::start(&roots, REVIEW_WATCH_DEBOUNCE, move || {
            let _ = tx.try_send(());
        }) {
            Ok(watcher) => {
                self.watcher = Some((roots, watcher));
                self.watch_task = Some(cx.spawn(async move |this, cx| {
                    while rx.recv().await.is_ok() {
                        if this
                            .update(cx, |this, cx| this.request_listing(cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                }));
            }
            Err(e) => {
                tracing::warn!("Review panel: change watcher unavailable: {e:#}");
                self.watcher = None;
                self.watch_task = None;
            }
        }
    }

    fn request_listing(&self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        if let Some(gpui) = cx.try_global::<Gpui>() {
            gpui.cmd_list_review_files(session_id, self.mode, self.base_overrides.clone());
        }
    }

    /// Request the next file that has no prepared diff yet, starting with the
    /// ones in view. At most one request is in flight; collapsed repos and files are skipped, which
    /// keeps loading lazy.
    fn ensure_diff_request(&mut self, cx: &mut Context<Self>) {
        if self.in_flight.is_some() {
            return;
        }
        let Some(session_id) = self.session_id.clone() else {
            return;
        };

        // A diff loaded for exactly this listing entry is current; one loaded
        // for an older entry (fingerprint moved) is stale and gets requested
        // again.
        let needs_load = |section: &RepoSection, file: &ChangedFile| {
            let key = (section.repo_root.clone(), file.path.clone());
            !section.collapsed
                && !self.collapsed_files.contains(&key)
                && !self
                    .file_diffs
                    .get(&key)
                    .is_some_and(|loaded| &loaded.file == file)
        };

        // Files in view first. The rows can lag behind a fresh listing, so
        // their indices are only hints; listing order covers the rest.
        let top_row = self.list_state.logical_scroll_top().item_ix;
        let by_proximity = files_by_proximity(&self.rows, top_row)
            .into_iter()
            .filter_map(|(repo, file)| {
                let section = self.repos.get(repo)?;
                Some((section, section.files.get(file)?))
            });
        let in_listing_order = self
            .repos
            .iter()
            .flat_map(|section| section.files.iter().map(move |file| (section, file)));
        let next = by_proximity
            .chain(in_listing_order)
            .find(|(section, file)| needs_load(section, file))
            .map(|(section, file)| {
                (
                    section.repo_root.clone(),
                    section.base.clone(),
                    file.clone(),
                )
            });

        if let Some((repo_root, base, file)) = next {
            self.in_flight = Some(((repo_root.clone(), file.path.clone()), file.clone()));
            if let Some(gpui) = cx.try_global::<Gpui>() {
                gpui.cmd_get_review_file_diff(session_id, repo_root, self.mode, base, file);
            }
        }
    }

    fn on_repo_base_changed(&mut self, repo_root: PathBuf, branch: String, cx: &mut Context<Self>) {
        // Diffs of this repo were computed against the old base.
        self.file_diffs.retain(|(root, _), _| root != &repo_root);
        self.in_flight = None;

        self.base_overrides.insert(repo_root, branch.clone());
        // Remember this as the global default for future repos/sessions.
        self.default_base = Some(branch.clone());
        crate::update_ui_settings(cx, |s| s.review_default_base = Some(branch));
        self.request_listing(cx);
        cx.notify();
    }

    fn on_mode_event(
        &mut self,
        _: &Entity<SelectState<Vec<ModeOption>>>,
        event: &SelectEvent<Vec<ModeOption>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let SelectEvent::Confirm(Some(mode)) = event
            && *mode != self.mode
        {
            self.mode = *mode;
            // A new mode invalidates every prepared diff.
            self.file_diffs.clear();
            self.in_flight = None;
            self.persist_mode(cx);
            self.request_listing(cx);
            cx.notify();
        }
    }

    /// Consume the latest listing from the global if it changed. The per-frame
    /// unchanged case is a generation compare — no clone, no deep equality.
    fn sync_listing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((generation, listing)) = cx
            .try_global::<Gpui>()
            .and_then(|g| g.review_listing_if_newer(self.listing_generation))
        else {
            return;
        };
        self.listing_generation = generation;

        let Some(listing) = listing else {
            // Cleared (e.g. session change): drop all sections.
            self.has_listing = false;
            self.repos.clear();
            return;
        };

        self.has_listing = true;
        self.is_git_repo = listing.is_git_repo;
        self.mode = listing.mode;

        // Sync the mode selector to the echoed mode.
        self.mode_state.update(cx, |state, cx| {
            state.set_selected_value(&listing.mode, window, cx);
        });

        // Rebuild sections when the set of repos changes; otherwise update the
        // existing sections in place (preserving expansion state).
        let incoming_roots: Vec<PathBuf> =
            listing.repos.iter().map(|r| r.repo_root.clone()).collect();
        let current_roots: Vec<PathBuf> = self.repos.iter().map(|r| r.repo_root.clone()).collect();

        if incoming_roots != current_roots {
            self.repos = listing
                .repos
                .iter()
                .map(|r| self.build_section(r, window, cx))
                .collect();
        } else {
            for (section, data) in self.repos.iter_mut().zip(listing.repos.iter()) {
                Self::update_section(section, data, window, cx);
            }
        }

        // Drop prepared diffs and collapse state of files that vanished from
        // the listing.
        let live: HashSet<FileKey> = self
            .repos
            .iter()
            .flat_map(|s| {
                s.files
                    .iter()
                    .map(|f| (s.repo_root.clone(), f.path.clone()))
            })
            .collect();
        self.file_diffs.retain(|key, _| live.contains(key));
        self.collapsed_files.retain(|key| live.contains(key));
        if let Some((in_flight, _)) = &self.in_flight
            && !live.contains(in_flight)
        {
            self.in_flight = None;
        }

        // Apply the persisted default base to any repo that has no explicit
        // override yet and whose resolved base differs. Seeding the override
        // and re-requesting makes the default actually take effect. This
        // terminates: once the backend echoes the default as the repo's base,
        // the `base != default` guard stops further seeding.
        if let Some(default) = self.default_base.clone() {
            let mut seeded = false;
            for data in &listing.repos {
                if !self.base_overrides.contains_key(&data.repo_root)
                    && data.base_candidates.iter().any(|c| c == &default)
                    && data.base.as_deref() != Some(default.as_str())
                {
                    self.base_overrides
                        .insert(data.repo_root.clone(), default.clone());
                    seeded = true;
                }
            }
            if seeded {
                self.request_listing(cx);
            }
        }

        self.ensure_watcher(cx);
        self.ensure_diff_request(cx);
    }

    /// Consume the latest diff arrival from the global if it changed. Only the
    /// response to the outstanding request is accepted; the hunks are computed
    /// once here, then the next missing diff is requested.
    fn sync_diff(&mut self, cx: &mut Context<Self>) {
        let Some((generation, diff)) = cx
            .try_global::<Gpui>()
            .and_then(|g| g.review_diff_if_newer(self.diff_generation))
        else {
            return;
        };
        self.diff_generation = generation;

        if let Some(d) = diff {
            let key = (d.repo_root, d.path);
            if let Some((_, file)) = self.in_flight.take_if(|(k, _)| *k == key) {
                self.next_diff_stamp += 1;
                self.file_diffs.insert(
                    key,
                    LoadedDiff {
                        file,
                        prepared: d.prepared,
                        stamp: self.next_diff_stamp,
                    },
                );
            }
        }
        self.ensure_diff_request(cx);
    }

    /// Create a fresh [`RepoSection`] for `data`, wiring the base-selector
    /// subscription so events identify their repo.
    fn build_section(
        &self,
        data: &RepoReviewData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> RepoSection {
        let items: Vec<BaseOption> = data
            .base_candidates
            .iter()
            .map(|b| BaseOption { branch: b.clone() })
            .collect();
        let effective_base = self.effective_base(data);
        let base_state = cx.new(|cx| {
            let mut state = SelectState::new(items, None, window, cx);
            if let Some(base) = &effective_base {
                state.set_selected_value(base, window, cx);
            }
            state
        });

        let root_for_base = data.repo_root.clone();
        let base_sub = cx.subscribe_in(
            &base_state,
            window,
            move |this, _state, event, _window, cx| {
                if let SelectEvent::Confirm(Some(branch)) = event {
                    this.on_repo_base_changed(root_for_base.clone(), branch.clone(), cx);
                }
            },
        );

        // Sections default to collapsed; only repos the user expanded (stored
        // by absolute root path in the UI settings) start open.
        let expanded = cx
            .try_global::<crate::UiSettingsGlobal>()
            .is_some_and(|g| g.0.review_expanded_repos.contains(&data.repo_root));

        RepoSection {
            repo_root: data.repo_root.clone(),
            label: data.label.clone(),
            base_state,
            base_candidates: data.base_candidates.clone(),
            base: effective_base,
            files: data.files.clone(),
            stats: data.stats,
            scan_state: data.scan_state,
            collapsed: !expanded,
            _base_sub: base_sub,
        }
    }

    /// Update an existing section's data + base selector in place.
    fn update_section(
        section: &mut RepoSection,
        data: &RepoReviewData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        section.label = data.label.clone();
        section.files = data.files.clone();
        section.stats = data.stats;
        section.scan_state = data.scan_state;

        if section.base_candidates != data.base_candidates {
            section.base_candidates = data.base_candidates.clone();
            let items: Vec<BaseOption> = data
                .base_candidates
                .iter()
                .map(|b| BaseOption { branch: b.clone() })
                .collect();
            section.base_state.update(cx, |state, cx| {
                state.set_items(items, window, cx);
            });
        }
        section.base = data.base.clone();
        if let Some(base) = &data.base {
            section.base_state.update(cx, |state, cx| {
                state.set_selected_value(base, window, cx);
            });
        }
    }

    /// The base ref to preselect for a repo: an explicit session override, else
    /// the persisted default (when it is a candidate), else the resolved base.
    fn effective_base(&self, data: &RepoReviewData) -> Option<String> {
        if let Some(base) = self.base_overrides.get(&data.repo_root) {
            return Some(base.clone());
        }
        if let Some(default) = &self.default_base
            && data.base_candidates.iter().any(|c| c == default)
        {
            return Some(default.clone());
        }
        data.base.clone()
    }

    /// The rotating double-arrow used on active sessions, in grey — shown on
    /// the repo whose scan is currently running. `id` keys the animation.
    fn scan_spinner(id: impl std::fmt::Display, muted: gpui_kit::Hsla) -> gpui_kit::AnyElement {
        gpui_kit::svg()
            .size(px(12.))
            .path("icons/arrow_circle.svg")
            .text_color(muted)
            .with_animation(
                gpui_kit::SharedString::from(format!("review-scan-spin-{id}")),
                gpui_kit::Animation::new(std::time::Duration::from_secs(2)).repeat(),
                |svg, delta| {
                    svg.with_transformation(gpui_kit::Transformation::rotate(gpui_kit::percentage(
                        delta,
                    )))
                },
            )
            .into_any_element()
    }

    /// The same double-arrow, static and faded — marks a repo that is queued
    /// for scanning but not yet running.
    fn pending_marker(muted: gpui_kit::Hsla) -> gpui_kit::AnyElement {
        gpui_kit::svg()
            .size(px(12.))
            .path("icons/arrow_circle.svg")
            .text_color(muted.opacity(0.4))
            .into_any_element()
    }

    /// The repo header's right-hand slot: a spinner while a repo is being
    /// scanned, a faded static one while it waits its turn, and a `+adds −dels`
    /// summary once its (possibly cached) result is in.
    fn render_scan_indicator(
        &self,
        section: &RepoSection,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;

        if matches!(section.scan_state, ReviewScanState::Scanning) {
            return Self::scan_spinner(section.repo_root.display(), muted);
        }

        let pending = matches!(section.scan_state, ReviewScanState::Pending);
        let has_data = !section.files.is_empty() || section.stats != git::DiffStats::default();
        if !has_data {
            // Nothing (yet) to summarize: a queued repo shows a wait marker,
            // a scanned clean repo shows no indicator at all.
            return if pending {
                Self::pending_marker(muted)
            } else {
                div().into_any_element()
            };
        }

        // Stats badge; a trailing wait marker means "cached, refresh queued".
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .text_xs()
            .child(
                div()
                    .text_color(added_row_colors(theme).1)
                    .child(format!("+{}", section.stats.additions)),
            )
            .child(
                div()
                    .text_color(deleted_row_colors(theme).1)
                    .child(format!("−{}", section.stats.deletions)),
            )
            .when(pending, |el| el.child(Self::pending_marker(muted)))
            .into_any_element()
    }

    /// Bring `rows` and the list in step with the current state. Only the
    /// span that changed is spliced, so everything else keeps its measured
    /// height and the scroll position holds.
    fn sync_rows(&mut self) {
        let base_selector = matches!(self.mode, ReviewMode::BranchVsBase);
        let outline: Vec<RepoOutline> = self
            .repos
            .iter()
            .map(|section| RepoOutline {
                collapsed: section.collapsed,
                base_selector,
                files: section
                    .files
                    .iter()
                    .map(|file| {
                        let key = (section.repo_root.clone(), file.path.clone());
                        FileOutline {
                            collapsed: self.collapsed_files.contains(&key),
                            diff: self.file_diffs.get(&key).map(|loaded| {
                                let prepared = &loaded.prepared;
                                let body = if prepared.is_binary || prepared.too_large {
                                    DiffBody::Nothing
                                } else if prepared.hunks.is_empty() {
                                    DiffBody::NoChanges
                                } else {
                                    DiffBody::Chunks(prepared.chunked.chunks.len())
                                };
                                (loaded.stamp, body)
                            }),
                        }
                    })
                    .collect(),
            })
            .collect();

        let rows = flatten(&outline);
        if let Some((old_range, count)) = changed_span(&self.rows, &rows) {
            self.list_state.splice(old_range, count);
        }
        self.rows = rows;
    }

    /// Build the list item at `ix`.
    fn render_row(
        &self,
        ix: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let empty = || div().into_any_element();
        let Some(row) = self.rows.get(ix) else {
            return empty();
        };
        match *row {
            ReviewRow::RepoHeader { repo } => match self.repos.get(repo) {
                Some(section) => self.render_repo_header(repo, section, cx),
                None => empty(),
            },
            ReviewRow::BaseSelector { repo } => match self.repos.get(repo) {
                Some(section) => self.render_base_selector(section, cx),
                None => empty(),
            },
            ReviewRow::FileHeader { repo, file } => {
                match self
                    .repos
                    .get(repo)
                    .and_then(|s| Some((s, s.files.get(file)?)))
                {
                    Some((section, file)) => self.render_file_header(&section.repo_root, file, cx),
                    None => empty(),
                }
            }
            ReviewRow::NoChanges { .. } => div()
                .px_3()
                .py_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("No content changes")
                .into_any_element(),
            ReviewRow::Chunk {
                repo, file, chunk, ..
            } => {
                let prepared = self.repos.get(repo).and_then(|section| {
                    let file = section.files.get(file)?;
                    let key = (section.repo_root.clone(), file.path.clone());
                    Some(&self.file_diffs.get(&key)?.prepared)
                });
                match prepared {
                    Some(prepared) => Self::render_chunk(prepared, chunk, window, cx),
                    None => empty(),
                }
            }
        }
    }

    /// A repo's collapsible header. Sections are separated by a line ABOVE
    /// each header (not by a line between a header and its content).
    fn render_repo_header(
        &self,
        ix: usize,
        section: &RepoSection,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let chevron = if section.collapsed {
            "icons/chevron_right.svg"
        } else {
            "icons/chevron_down.svg"
        };
        let toggle_root = section.repo_root.clone();
        div()
            .id(gpui_kit::SharedString::from(format!("repo-header-{ix}")))
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap_1p5()
            .px_2()
            .py_1()
            .when(ix > 0, |s| s.border_t_1().border_color(theme.border))
            .cursor_pointer()
            .hover(|s| s.bg(theme.muted))
            .child(
                gpui_kit::svg()
                    .size(px(12.))
                    .path(chevron)
                    .text_color(muted),
            )
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(section.label.clone()),
            )
            .child(self.render_scan_indicator(section, cx))
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                let Some(s) = this.repos.iter_mut().find(|s| s.repo_root == toggle_root) else {
                    return;
                };
                s.collapsed = !s.collapsed;
                let expanded = !s.collapsed;

                // Persist per repo root (sections default to collapsed).
                let root = toggle_root.clone();
                crate::update_ui_settings(cx, move |settings| {
                    if expanded {
                        if !settings.review_expanded_repos.contains(&root) {
                            settings.review_expanded_repos.push(root);
                        }
                    } else {
                        settings.review_expanded_repos.retain(|r| r != &root);
                    }
                });

                // Expanding may unlock diffs skipped while collapsed.
                this.ensure_diff_request(cx);
                cx.notify();
            }))
            .into_any_element()
    }

    /// The per-repo base selector (branch mode only).
    fn render_base_selector(
        &self,
        section: &RepoSection,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        div()
            .px_2()
            .py_1()
            .child(
                Select::new(&section.base_state)
                    .placeholder("Base")
                    .with_size(Size::XSmall)
                    .icon(
                        Icon::default()
                            .path("icons/chevron_up_down.svg")
                            .with_size(Size::XSmall)
                            .text_color(cx.theme().muted_foreground),
                    )
                    .w_full(),
            )
            .into_any_element()
    }

    /// One file's collapsible header (icon, path, status, `+/−`); its diff
    /// chunks follow as separate rows.
    fn render_file_header(
        &self,
        repo_root: &std::path::Path,
        file: &ChangedFile,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let fg = theme.foreground;
        let border = theme.border;

        let key: FileKey = (repo_root.to_path_buf(), file.path.clone());
        let collapsed = self.collapsed_files.contains(&key);
        let entry = self.file_diffs.get(&key).map(|loaded| &loaded.prepared);
        let loading = self.in_flight.as_ref().is_some_and(|(k, _)| *k == key);

        // Right-hand slot of the file header.
        let indicator: gpui_kit::AnyElement = match entry {
            Some(e) if e.is_binary => div()
                .text_xs()
                .text_color(muted)
                .child("binary")
                .into_any_element(),
            Some(e) if e.too_large => div()
                .text_xs()
                .text_color(muted)
                .child("too large")
                .into_any_element(),
            Some(e) => div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .text_xs()
                .child(
                    div()
                        .text_color(added_row_colors(theme).1)
                        .child(format!("+{}", e.additions)),
                )
                .child(
                    div()
                        .text_color(deleted_row_colors(theme).1)
                        .child(format!("−{}", e.deletions)),
                )
                .into_any_element(),
            None if loading => {
                Self::scan_spinner(format!("{}:{}", repo_root.display(), file.path), muted)
            }
            None => Self::pending_marker(muted),
        };

        let chevron = if collapsed {
            "icons/chevron_right.svg"
        } else {
            "icons/chevron_down.svg"
        };
        let (status_letter, status_color) = status_badge(file.status);
        let file_name = file.path.rsplit('/').next().unwrap_or(&file.path);
        let icon = file_icons::get().get_icon_for_filename(file_name);

        let toggle_key = key.clone();
        div()
            .id(gpui_kit::SharedString::from(format!(
                "review-file-{}:{}",
                repo_root.display(),
                file.path
            )))
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap_1p5()
            .pl_3()
            .pr_2()
            .py_0p5()
            .border_t_1()
            .border_color(border)
            .cursor_pointer()
            .hover(|s| s.bg(theme.muted))
            .child(
                gpui_kit::svg()
                    .size(px(10.))
                    .path(chevron)
                    .text_color(muted),
            )
            .child(file_icons::render_icon(&icon, 14.0, muted, "📄"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_xs()
                    .text_color(fg)
                    .child(file.path.clone()),
            )
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(status_color)
                    .child(status_letter),
            )
            .child(indicator)
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                if !this.collapsed_files.remove(&toggle_key) {
                    this.collapsed_files.insert(toggle_key.clone());
                }
                // Expanding may unlock a diff that was skipped while collapsed.
                this.ensure_diff_request(cx);
                cx.notify();
            }))
            .into_any_element()
    }

    /// One chunk of a file's diff body. The first and last chunk carry the
    /// body's vertical padding, so the chunks read as one block.
    fn render_chunk(
        prepared: &PreparedReviewDiff,
        chunk_ix: usize,
        window: &Window,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        let chunks = &prepared.chunked.chunks;
        let Some(chunk) = chunks.get(chunk_ix) else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let rem_size = window.rem_size();
        let is_dark = theme.background.l < 0.5;
        let body_bg = if is_dark {
            gpui_kit::hsla(0.0, 0.0, 0.08, 1.0)
        } else {
            gpui_kit::hsla(0.0, 0.0, 0.97, 1.0)
        };
        let line_height_px = rems(1.25).to_pixels(rem_size).round();
        div()
            .w_full()
            .when(chunk_ix == 0, |d| d.pt_1())
            .when(chunk_ix + 1 == chunks.len(), |d| d.pb_1())
            .bg(body_bg)
            .flex()
            .flex_col()
            .text_size(rems(0.78125))
            .line_height(line_height_px)
            .font_family("Menlo")
            .font_weight(FontWeight(400.0))
            .child(render_diff_chunk(
                &prepared.hunks,
                chunk,
                prepared.chunked.gutter_width,
                prepared.syntax.as_deref(),
                theme,
                rem_size,
            ))
            .into_any_element()
    }
}

impl Focusable for ReviewView {
    fn focus_handle(&self, _cx: &gpui_kit::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ReviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Pull fresh backend data before laying out. Both syncs are a cheap
        // generation compare when nothing changed.
        self.sync_listing(window, cx);
        self.sync_diff(cx);

        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;

        // Before the first (fast) discovery response there is nothing to lay
        // out yet — show explicit activity instead of an empty panel.
        if !self.has_listing {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .p_4()
                .text_sm()
                .text_color(muted)
                .child(Self::scan_spinner("discovery", muted))
                .child("Looking for repositories…")
                .into_any_element();
        }

        if !self.is_git_repo {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .p_4()
                .text_sm()
                .text_color(muted)
                .child("Not a git repository")
                .into_any_element();
        }

        // Header: compare-mode selector only (base selectors live per repo).
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .p_2()
            .border_b_1()
            .border_color(border)
            .child(
                Select::new(&self.mode_state)
                    .placeholder("Compare")
                    .with_size(Size::XSmall)
                    .icon(
                        Icon::default()
                            .path("icons/chevron_up_down.svg")
                            .with_size(Size::XSmall)
                            .text_color(muted),
                    )
                    .min_w(px(130.)),
            );

        // The render callback only runs for rows in (or near) the viewport.
        self.sync_rows();
        let body = list(
            self.list_state.clone(),
            cx.processor(|this: &mut Self, ix: usize, window, cx| this.render_row(ix, window, cx)),
        )
        .size_full();

        v_flex()
            .size_full()
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(body)
                    .vertical_scrollbar(&self.list_state),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{TestAppContext, VisualTestContext};

    fn added_file(path: &str) -> ChangedFile {
        ChangedFile {
            path: path.into(),
            orig_path: None,
            status: ChangeStatus::Added,
            fingerprint: None,
        }
    }

    /// A pure-add diff of `lines` lines.
    fn prepared(lines: usize) -> PreparedReviewDiff {
        let text: String = (0..lines).map(|i| format!("line {i}\n")).collect();
        PreparedReviewDiff::from_content(
            "a.rs",
            &git::FileDiffContent {
                old_text: None,
                new_text: Some(text),
                is_binary: false,
                too_large: false,
            },
        )
    }

    /// A window with a `ReviewView` showing one expanded repo with `files`.
    fn view_with_files(
        files: Vec<ChangedFile>,
        cx: &mut TestAppContext,
    ) -> (Entity<ReviewView>, &mut VisualTestContext) {
        let window = cx.update(|cx| {
            gpui_kit::component::init(cx);
            file_icons::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| ReviewView::new(window, cx))
            })
            .unwrap()
        });
        let view = window.root(cx).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        view.update_in(cx, |view, window, cx| {
            let data = RepoReviewData {
                repo_root: PathBuf::from("/repo"),
                label: "repo".into(),
                current_branch: None,
                base_candidates: Vec::new(),
                base: None,
                files,
                stats: git::DiffStats::default(),
                scan_state: ReviewScanState::Done,
            };
            let mut section = view.build_section(&data, window, cx);
            section.collapsed = false;
            view.repos = vec![section];
            view.has_listing = true;
            view.is_git_repo = true;
        });
        (view, cx)
    }

    #[gpui_kit::test]
    fn highlighted_rows_with_word_emphasis_render(cx: &mut TestAppContext) {
        let (view, cx) = view_with_files(vec![added_file("a.rs")], cx);
        let prepared = PreparedReviewDiff::from_content(
            "a.rs",
            &git::FileDiffContent {
                old_text: Some("fn grüße() -> &'static str { \"hallo wält\" }\n".into()),
                new_text: Some("fn grüße() -> &'static str { \"hallo wörld\" }\n".into()),
                is_binary: false,
                too_large: false,
            },
        );
        assert!(prepared.syntax.is_some());
        assert!(
            prepared.hunks[0]
                .lines
                .iter()
                .any(|l| !l.emphasis.is_empty())
        );

        view.update(cx, |view, cx| {
            view.file_diffs.insert(
                (PathBuf::from("/repo"), "a.rs".into()),
                LoadedDiff {
                    file: added_file("a.rs"),
                    prepared,
                    stamp: 1,
                },
            );
            cx.notify();
        });
        // Drawing must cope with syntax and emphasis ranges overlapping.
        cx.run_until_parked();
        view.update(cx, |view, _| assert_eq!(view.list_state.item_count(), 3));
    }

    #[gpui_kit::test]
    fn list_items_follow_loaded_diffs_and_collapse_state(cx: &mut TestAppContext) {
        let root = PathBuf::from("/repo");
        let (view, cx) = view_with_files(vec![added_file("a.rs"), added_file("b.rs")], cx);

        view.update(cx, |view, cx| {
            // Three chunks' worth of lines for a.rs; b.rs has no diff yet.
            view.file_diffs.insert(
                (root.clone(), "a.rs".into()),
                LoadedDiff {
                    file: added_file("a.rs"),
                    prepared: prepared(100),
                    stamp: 1,
                },
            );
            cx.notify();
        });
        cx.run_until_parked();

        // Repo header + 2 file headers + 3 chunks.
        view.update(cx, |view, _| {
            assert_eq!(view.rows.len(), 6);
            assert_eq!(view.list_state.item_count(), 6);
        });

        view.update(cx, |view, cx| {
            view.collapsed_files.insert((root.clone(), "a.rs".into()));
            cx.notify();
        });
        cx.run_until_parked();

        view.update(cx, |view, _| {
            assert_eq!(view.list_state.item_count(), 3);
            assert_eq!(
                view.rows,
                vec![
                    ReviewRow::RepoHeader { repo: 0 },
                    ReviewRow::FileHeader { repo: 0, file: 0 },
                    ReviewRow::FileHeader { repo: 0, file: 1 },
                ]
            );
        });
    }
}
