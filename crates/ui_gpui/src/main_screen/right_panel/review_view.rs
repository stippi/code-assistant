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
//! Diffs load lazily, one file at a time: after each arrival the next visible
//! file without a diff is requested. Hunks (changed lines + a few context
//! lines) are computed once on arrival and cached — rendering never diffs, and
//! the element count scales with changed lines, not file sizes.

use crate::shared::file_icons;
use crate::tool_cards::diff_card::{added_row_colors, deleted_row_colors, render_diff_hunks};
use crate::{Gpui, PreparedReviewDiff, RepoReviewData};
use code_assistant_core::session::{ReviewMode, ReviewScanState};
use git::{ChangeStatus, ChangedFile};
use gpui::{
    AnimationExt, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Render,
    Subscription, Window, div, prelude::*, px, rems,
};
use gpui_component::{
    ActiveTheme, Icon, Sizable, Size,
    scroll::ScrollableElement,
    select::{Select, SelectEvent, SelectItem, SelectState},
    v_flex,
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
    fn title(&self) -> gpui::SharedString {
        self.label.clone().into()
    }
    fn display_title(&self) -> Option<gpui::AnyElement> {
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
    fn title(&self) -> gpui::SharedString {
        self.branch.clone().into()
    }
    fn display_title(&self) -> Option<gpui::AnyElement> {
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
fn status_badge(status: ChangeStatus) -> (&'static str, gpui::Hsla) {
    match status {
        ChangeStatus::Added | ChangeStatus::Untracked => ("A", gpui::rgb(0x3f_a5_5a).into()),
        ChangeStatus::Modified => ("M", gpui::rgb(0xc7_9a_3a).into()),
        ChangeStatus::Deleted => ("D", gpui::rgb(0xc7_4a_4a).into()),
        ChangeStatus::Renamed => ("R", gpui::rgb(0x4a_82_c7).into()),
        ChangeStatus::Copied => ("C", gpui::rgb(0x4a_82_c7).into()),
        ChangeStatus::TypeChanged => ("T", gpui::rgb(0x8a_6a_c7).into()),
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

    /// Prepared diffs by file, filled lazily one request at a time.
    file_diffs: HashMap<FileKey, PreparedReviewDiff>,
    /// Files the user collapsed (default is expanded).
    collapsed_files: HashSet<FileKey>,
    /// The single outstanding diff request; arrivals for anything else are
    /// stale (e.g. from before a mode/base change) and dropped.
    in_flight: Option<FileKey>,

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

    fn request_listing(&self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        if let Some(gpui) = cx.try_global::<Gpui>() {
            gpui.cmd_list_review_files(session_id, self.mode, self.base_overrides.clone());
        }
    }

    /// Request the next visible file that has no prepared diff yet. At most
    /// one request is in flight; collapsed repos and files are skipped, which
    /// keeps loading lazy.
    fn ensure_diff_request(&mut self, cx: &mut Context<Self>) {
        if self.in_flight.is_some() {
            return;
        }
        let Some(session_id) = self.session_id.clone() else {
            return;
        };

        let mut next: Option<(PathBuf, Option<String>, ChangedFile)> = None;
        'outer: for section in &self.repos {
            if section.collapsed {
                continue;
            }
            for file in &section.files {
                let key = (section.repo_root.clone(), file.path.clone());
                if self.collapsed_files.contains(&key) || self.file_diffs.contains_key(&key) {
                    continue;
                }
                next = Some((
                    section.repo_root.clone(),
                    section.base.clone(),
                    file.clone(),
                ));
                break 'outer;
            }
        }

        if let Some((repo_root, base, file)) = next {
            self.in_flight = Some((repo_root.clone(), file.path.clone()));
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
        if let Some(in_flight) = &self.in_flight
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
            if self.in_flight.as_ref() == Some(&key) {
                self.in_flight = None;
                self.file_diffs.insert(key, d.prepared);
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
    fn scan_spinner(id: impl std::fmt::Display, muted: gpui::Hsla) -> gpui::AnyElement {
        gpui::svg()
            .size(px(12.))
            .path("icons/arrow_circle.svg")
            .text_color(muted)
            .with_animation(
                gpui::SharedString::from(format!("review-scan-spin-{id}")),
                gpui::Animation::new(std::time::Duration::from_secs(2)).repeat(),
                |svg, delta| {
                    svg.with_transformation(gpui::Transformation::rotate(gpui::percentage(delta)))
                },
            )
            .into_any_element()
    }

    /// The same double-arrow, static and faded — marks a repo that is queued
    /// for scanning but not yet running.
    fn pending_marker(muted: gpui::Hsla) -> gpui::AnyElement {
        gpui::svg()
            .size(px(12.))
            .path("icons/arrow_circle.svg")
            .text_color(muted.opacity(0.4))
            .into_any_element()
    }

    /// The repo header's right-hand slot: a spinner while a repo is being
    /// scanned, a faded static one while it waits its turn, and a `+adds −dels`
    /// summary once its (possibly cached) result is in.
    fn render_scan_indicator(&self, section: &RepoSection, cx: &Context<Self>) -> gpui::AnyElement {
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

    /// One stacked file: collapsible header (icon, path, status, `+/−`) with
    /// the file's diff hunks directly below.
    fn render_file_entry(
        &self,
        repo_root: &std::path::Path,
        file: &ChangedFile,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let fg = theme.foreground;
        let border = theme.border;

        let key: FileKey = (repo_root.to_path_buf(), file.path.clone());
        let collapsed = self.collapsed_files.contains(&key);
        let entry = self.file_diffs.get(&key);
        let loading = self.in_flight.as_ref() == Some(&key);

        // Right-hand slot of the file header.
        let indicator: gpui::AnyElement = match entry {
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
        let header = div()
            .id(gpui::SharedString::from(format!(
                "review-file-{}:{}",
                repo_root.display(),
                file.path
            )))
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
            .child(gpui::svg().size(px(10.)).path(chevron).text_color(muted))
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
            }));

        let mut container = v_flex().w_full().child(header);

        if !collapsed && let Some(entry) = entry {
            let body: Option<gpui::AnyElement> = if entry.is_binary || entry.too_large {
                None // The header badge already says why there is no diff.
            } else if entry.hunks.is_empty() {
                Some(
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(muted)
                        .child("No content changes")
                        .into_any_element(),
                )
            } else {
                let rem_size = window.rem_size();
                let is_dark = theme.background.l < 0.5;
                let body_bg = if is_dark {
                    gpui::hsla(0.0, 0.0, 0.08, 1.0)
                } else {
                    gpui::hsla(0.0, 0.0, 0.97, 1.0)
                };
                let line_height_px = rems(1.25).to_pixels(rem_size).round();
                Some(
                    div()
                        .w_full()
                        .py_1()
                        .bg(body_bg)
                        .flex()
                        .flex_col()
                        .text_size(rems(0.78125))
                        .line_height(line_height_px)
                        .font_family("Menlo")
                        .font_weight(FontWeight(400.0))
                        .child(render_diff_hunks(&entry.hunks, theme, rem_size))
                        .into_any_element(),
                )
            };
            if let Some(body) = body {
                container = container.child(body);
            }
        }

        container.into_any_element()
    }

    /// The scrollable stack of per-repo sections with their stacked files.
    fn render_sections(&mut self, window: &Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let border = cx.theme().border;
        let branch_mode = matches!(self.mode, ReviewMode::BranchVsBase);

        let mut column = v_flex().size_full().overflow_y_scrollbar();

        // Snapshot the per-section data needed while building children, so the
        // listener closures (which borrow `this`) don't fight the loop borrow.
        let section_count = self.repos.len();
        for ix in 0..section_count {
            let (repo_root, label, collapsed, scan_state) = {
                let s = &self.repos[ix];
                (
                    s.repo_root.clone(),
                    s.label.clone(),
                    s.collapsed,
                    s.scan_state,
                )
            };
            let _ = scan_state;

            // Sections are separated by a line ABOVE each section (not by a
            // line between a section's header and its content).
            let mut section_el = v_flex()
                .w_full()
                .when(ix > 0, |s| s.border_t_1().border_color(border));

            let chevron = if collapsed {
                "icons/chevron_right.svg"
            } else {
                "icons/chevron_down.svg"
            };
            let toggle_root = repo_root.clone();
            let header = div()
                .id(gpui::SharedString::from(format!("repo-header-{ix}")))
                .flex()
                .flex_row()
                .items_center()
                .gap_1p5()
                .px_2()
                .py_1()
                .cursor_pointer()
                .hover(|s| s.bg(cx.theme().muted))
                .child(gpui::svg().size(px(12.)).path(chevron).text_color(muted))
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(fg)
                        .child(label),
                )
                .child(self.render_scan_indicator(&self.repos[ix], cx))
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
                }));
            section_el = section_el.child(header);

            if !collapsed {
                if branch_mode {
                    section_el = section_el.child(
                        div().px_2().py_1().child(
                            Select::new(&self.repos[ix].base_state)
                                .placeholder("Base")
                                .with_size(Size::XSmall)
                                .icon(
                                    Icon::default()
                                        .path("icons/chevron_up_down.svg")
                                        .with_size(Size::XSmall)
                                        .text_color(muted),
                                )
                                .w_full(),
                        ),
                    );
                }
                // A repo without changes shows just its header — the missing
                // +/− badge already says "clean".
                let files = self.repos[ix].files.clone();
                for file in &files {
                    section_el =
                        section_el.child(self.render_file_entry(&repo_root, file, window, cx));
                }
            }

            column = column.child(section_el);
        }

        column.into_any_element()
    }
}

impl Focusable for ReviewView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
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

        let body = self.render_sections(window, cx);

        v_flex()
            .size_full()
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}
