//! The "Review" view for the right sidebar: a compare-mode selector plus a
//! two-column body — the unified diff on the **left** and, on the **right**, a
//! per-repo set of changed-file trees (each repo gets its own base-branch
//! selector in "Branch vs base" mode). The two columns are separated by a
//! draggable divider (`h_resizable`).
//!
//! Backend data arrives through the `current_review_listing` / `current_review_diff`
//! globals on [`Gpui`]; this view consumes them in `render` by diffing against
//! cached copies (the same "sync-in-render" technique the worktree selector uses).

use crate::shared::file_tree::{ChangedFilesTree, ChangedFilesTreeEvent};
use crate::tool_cards::diff_card::render_unified_diff;
use crate::{Gpui, ReviewData};
use code_assistant_core::session::ReviewMode;
use gpui::{
    Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Render, ScrollHandle,
    Subscription, Window, div, prelude::*, px, rems,
};
use gpui_component::{
    ActiveTheme, Icon, Sizable, Size,
    resizable::{ResizableState, h_resizable, resizable_panel},
    scroll::ScrollableElement,
    select::{Select, SelectEvent, SelectItem, SelectState},
    v_flex,
};
use std::collections::HashMap;
use std::path::PathBuf;

/// Default width (px) of the file-tree column when nothing is persisted.
const DEFAULT_TREE_WIDTH: f32 = 240.0;

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

/// One git repo's changed-files tree plus its base selector, rendered as a
/// collapsible section in the right column.
struct RepoSection {
    repo_root: PathBuf,
    label: String,
    tree: Entity<ChangedFilesTree>,
    base_state: Entity<SelectState<Vec<BaseOption>>>,
    base_candidates: Vec<String>,
    base: Option<String>,
    collapsed: bool,
    _tree_sub: Subscription,
    _base_sub: Subscription,
}

// ---------------------------------------------------------------------------
// ReviewView
// ---------------------------------------------------------------------------

pub struct ReviewView {
    session_id: Option<String>,
    mode_state: Entity<SelectState<Vec<ModeOption>>>,

    /// Current compare mode (drives requests). Base is tracked per repo.
    mode: ReviewMode,
    is_git_repo: bool,

    /// Per-repo sections in the right column.
    repos: Vec<RepoSection>,
    /// Currently selected file as `(repo_root, repo-relative path)`.
    selected: Option<(PathBuf, String)>,
    /// User's explicit per-repo base choices for this session.
    base_overrides: HashMap<PathBuf, String>,
    /// Persisted default base ref, seeds a repo's base when it has no override.
    default_base: Option<String>,

    /// Two-column split state (LEFT = diff, RIGHT = tree column).
    split_state: Entity<ResizableState>,
    /// Persisted tree-column width, used as the initial panel size.
    tree_width: f32,
    /// Scroll position of the diff pane.
    diff_scroll: ScrollHandle,

    /// Last listing consumed from the global (change detection).
    last_listing: Option<ReviewData>,

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

        // Seed persisted preferences (default base + tree width) from settings.
        let (default_base, tree_width) = cx
            .try_global::<crate::UiSettingsGlobal>()
            .map(|g| {
                (
                    g.0.review_default_base.clone(),
                    g.0.review_tree_width.unwrap_or(DEFAULT_TREE_WIDTH),
                )
            })
            .unwrap_or((None, DEFAULT_TREE_WIDTH));

        let split_state = cx.new(|_| ResizableState::default());

        Self {
            session_id: None,
            mode_state,
            mode: ReviewMode::WorkingTree,
            is_git_repo: false,
            repos: Vec::new(),
            selected: None,
            base_overrides: HashMap::new(),
            default_base,
            split_state,
            tree_width,
            diff_scroll: ScrollHandle::new(),
            last_listing: None,
            focus_handle: cx.focus_handle(),
            _mode_sub: mode_sub,
        }
    }

    /// Point the view at a session and request its changed files.
    pub fn set_session(&mut self, session_id: Option<String>, cx: &mut Context<Self>) {
        self.session_id = session_id;
        // Reset per-session state; fresh data will arrive via the global.
        self.selected = None;
        self.last_listing = None;
        self.repos.clear();
        self.base_overrides.clear();

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

    fn request_diff(&self, repo_root: &PathBuf, path: &str, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let Some(listing) = &self.last_listing else {
            return;
        };
        let Some(repo) = listing.repos.iter().find(|r| &r.repo_root == repo_root) else {
            return;
        };
        let Some(file) = repo.files.iter().find(|f| f.path == path).cloned() else {
            return;
        };
        let base = repo.base.clone();
        if let Some(gpui) = cx.try_global::<Gpui>() {
            gpui.cmd_get_review_file_diff(session_id, repo_root.clone(), self.mode, base, file);
        }
    }

    fn on_file_selected(&mut self, repo_root: PathBuf, path: String, cx: &mut Context<Self>) {
        // Clear selection highlight in sibling repos' trees.
        for section in &self.repos {
            if section.repo_root != repo_root {
                section.tree.update(cx, |t, cx| t.set_selected(None, cx));
            }
        }
        self.request_diff(&repo_root, &path, cx);
        self.selected = Some((repo_root, path));
        cx.notify();
    }

    fn on_repo_base_changed(&mut self, repo_root: PathBuf, branch: String, cx: &mut Context<Self>) {
        self.base_overrides.insert(repo_root, branch.clone());
        // Remember this as the global default for future repos/sessions.
        self.default_base = Some(branch.clone());
        crate::update_ui_settings(cx, |s| s.review_default_base = Some(branch));
        self.selected = None;
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
            // Selecting a new mode invalidates the current diff selection.
            self.selected = None;
            self.persist_mode(cx);
            self.request_listing(cx);
            cx.notify();
        }
    }

    /// Consume the latest listing from the global if it changed.
    fn sync_listing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let listing = cx
            .try_global::<Gpui>()
            .and_then(|g| g.get_current_review_listing());

        if listing == self.last_listing {
            return;
        }
        self.last_listing = listing.clone();

        let Some(listing) = listing else {
            // Cleared (e.g. session change): drop all sections.
            self.repos.clear();
            return;
        };

        self.is_git_repo = listing.is_git_repo;
        self.mode = listing.mode;

        // Sync the mode selector to the echoed mode.
        self.mode_state.update(cx, |state, cx| {
            state.set_selected_value(&listing.mode, window, cx);
        });

        // Rebuild sections when the set of repos changes; otherwise update the
        // existing sections in place (preserving expansion / selection).
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

        // Reconcile the selection against the fresh listing.
        let still_present = self.selected.as_ref().is_some_and(|(root, path)| {
            listing
                .repos
                .iter()
                .any(|r| &r.repo_root == root && r.files.iter().any(|f| &f.path == path))
        });
        if !still_present {
            self.selected = None;
        }
        let selected = self.selected.clone();
        for section in &self.repos {
            let sel = selected
                .as_ref()
                .filter(|(root, _)| root == &section.repo_root)
                .map(|(_, path)| path.clone());
            section.tree.update(cx, |t, cx| t.set_selected(sel, cx));
        }
    }

    /// Create a fresh [`RepoSection`] for `data`, wiring per-repo subscriptions
    /// that capture the repo root so events identify their origin.
    fn build_section(
        &self,
        data: &crate::RepoReviewData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> RepoSection {
        let tree = cx.new(ChangedFilesTree::new);
        tree.update(cx, |t, cx| t.set_files(&data.files, cx));

        let root_for_tree = data.repo_root.clone();
        let tree_sub = cx.subscribe_in(&tree, window, move |this, _tree, event, _window, cx| {
            let ChangedFilesTreeEvent::FileSelected(path) = event;
            this.on_file_selected(root_for_tree.clone(), path.clone(), cx);
        });

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

        RepoSection {
            repo_root: data.repo_root.clone(),
            label: data.label.clone(),
            tree,
            base_state,
            base_candidates: data.base_candidates.clone(),
            base: effective_base,
            collapsed: false,
            _tree_sub: tree_sub,
            _base_sub: base_sub,
        }
    }

    /// Update an existing section's tree + base selector in place.
    fn update_section(
        section: &mut RepoSection,
        data: &crate::RepoReviewData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        section.label = data.label.clone();
        section
            .tree
            .update(cx, |t, cx| t.set_files(&data.files, cx));

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
    fn effective_base(&self, data: &crate::RepoReviewData) -> Option<String> {
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

    fn render_diff_pane(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;

        let placeholder = |msg: &str| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_sm()
                .text_color(muted)
                .child(msg.to_string())
                .into_any_element()
        };

        let Some((sel_root, sel_path)) = self.selected.clone() else {
            return placeholder("Select a file to view its diff");
        };

        let diff = cx
            .try_global::<Gpui>()
            .and_then(|g| g.get_current_review_diff());

        let Some(diff) = diff.filter(|d| d.repo_root == sel_root && d.path == sel_path) else {
            return placeholder("Loading diff…");
        };

        if diff.diff.is_binary {
            return placeholder("Binary file — no text diff");
        }
        if diff.diff.too_large {
            return placeholder("File too large to display");
        }

        let old = diff.diff.old_text.clone().unwrap_or_default();
        let new = diff.diff.new_text.clone().unwrap_or_default();
        if old.is_empty() && new.is_empty() {
            return placeholder("No changes to display");
        }

        let rem_size = window.rem_size();
        let theme = cx.theme();
        // Match the chat diff card's body styling so the unified diff (with its
        // red/green row backgrounds) renders identically here.
        let is_dark = theme.background.l < 0.5;
        let body_bg = if is_dark {
            gpui::hsla(0.0, 0.0, 0.08, 1.0)
        } else {
            gpui::hsla(0.0, 0.0, 0.97, 1.0)
        };
        let line_height_px = rems(1.25).to_pixels(rem_size).round();
        let diff = render_unified_diff(&old, &new, theme, Some(1), rem_size);

        div()
            .id("review-diff-scroll")
            .size_full()
            .overflow_scroll()
            .track_scroll(&self.diff_scroll)
            .child(
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
                    .child(diff),
            )
            .into_any_element()
    }

    /// Render the right column: a scrollable stack of per-repo sections.
    fn render_tree_column(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let border = cx.theme().border;
        let branch_mode = matches!(self.mode, ReviewMode::BranchVsBase);
        // A single repo whose label matches the project needn't show its header
        // chrome; but keeping it uniform is simpler and clarifies multi-repo.
        let show_headers = self.repos.len() > 1 || branch_mode;

        let mut column = v_flex().size_full().overflow_y_scrollbar();

        for (ix, section) in self.repos.iter().enumerate() {
            let repo_root = section.repo_root.clone();
            let collapsed = section.collapsed;

            let mut section_el = v_flex().w_full();

            if show_headers {
                let chevron = if collapsed {
                    "icons/chevron_right.svg"
                } else {
                    "icons/chevron_down.svg"
                };
                let header = div()
                    .id(gpui::SharedString::from(format!("repo-header-{ix}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1p5()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(border)
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().muted))
                    .child(gpui::svg().size(px(12.)).path(chevron).text_color(muted))
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(fg)
                            .child(section.label.clone()),
                    )
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        if let Some(s) = this.repos.iter_mut().find(|s| s.repo_root == repo_root) {
                            s.collapsed = !s.collapsed;
                            cx.notify();
                        }
                    }));
                section_el = section_el.child(header);
            }

            if !collapsed {
                if branch_mode {
                    section_el = section_el.child(
                        div().px_2().py_1().child(
                            Select::new(&section.base_state)
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
                section_el = section_el.child(section.tree.clone());
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
        // Pull fresh backend data before laying out.
        self.sync_listing(window, cx);

        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;

        if !self.is_git_repo && self.last_listing.is_some() {
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

        // Two-column body: diff (LEFT, grows) | tree column (RIGHT, sized).
        let diff_pane = self.render_diff_pane(window, cx);
        let tree_column = self.render_tree_column(cx);

        let body = h_resizable("review-split")
            .with_state(&self.split_state)
            .on_resize(|state, _window, cx| {
                if let Some(width) = state.read(cx).sizes().get(1).copied() {
                    let w = f32::from(width);
                    crate::update_ui_settings(cx, |s| s.review_tree_width = Some(w));
                }
            })
            .child(resizable_panel().child(diff_pane))
            .child(
                resizable_panel()
                    .size(px(self.tree_width))
                    .size_range(px(180.)..px(600.))
                    .flex_none()
                    .child(
                        div()
                            .size_full()
                            .border_l_1()
                            .border_color(border)
                            .child(tree_column),
                    ),
            );

        v_flex()
            .size_full()
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}
