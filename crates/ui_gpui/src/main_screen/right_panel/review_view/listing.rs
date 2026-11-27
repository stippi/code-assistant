//! What the Review tab compares and where: the session, the compare mode,
//! the base branch, the repository shown; the listing taken from the
//! `current_review_listing` global and the watcher that asks for a new
//! one.

use super::{GENERATION_UNSEEN, ReviewFile, ReviewView};
use crate::main_screen::right_panel::code_view::file::LoadedFile;
use crate::main_screen::right_panel::code_view::list::CodeHost;
use crate::main_screen::right_panel::repo_picker::{PickerRepo, RepoPicked, RepoPicker};
use crate::{Gpui, RepoReviewData};
use code_assistant_core::session::{ReviewMode, ReviewScanState};
use gpui_kit::component::button::Button;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size};
use gpui_kit::{
    AnyElement, App, Context, Entity, SharedString, Subscription, Window, div, prelude::*, px,
};
use std::path::{Path, PathBuf};

/// Quiet period the change watcher waits before reporting a burst of
/// filesystem events, so an edit (or a build) costs one scan.
const WATCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(300);

#[derive(Clone, Debug)]
pub(super) struct ModeOption {
    label: &'static str,
    value: ReviewMode,
}

impl SelectItem for ModeOption {
    type Value = ReviewMode;
    fn title(&self) -> SharedString {
        self.label.into()
    }
    fn display_title(&self) -> Option<AnyElement> {
        None
    }
    fn value(&self) -> &Self::Value {
        &self.value
    }
}

fn mode_options() -> Vec<ModeOption> {
    vec![
        ModeOption {
            label: "Working tree",
            value: ReviewMode::WorkingTree,
        },
        ModeOption {
            label: "Branch vs base",
            value: ReviewMode::BranchVsBase,
        },
        ModeOption {
            label: "Unpushed",
            value: ReviewMode::Unpushed,
        },
    ]
}

#[derive(Clone, Debug)]
pub(super) struct BaseOption {
    branch: String,
}

impl SelectItem for BaseOption {
    type Value = String;
    fn title(&self) -> SharedString {
        self.branch.clone().into()
    }
    fn display_title(&self) -> Option<AnyElement> {
        None
    }
    fn value(&self) -> &Self::Value {
        &self.branch
    }
}

type Controls = (
    Entity<SelectState<Vec<ModeOption>>>,
    Entity<SelectState<Vec<BaseOption>>>,
    Entity<RepoPicker<PathBuf>>,
    Vec<Subscription>,
);

/// The mode and base selectors and the repository picker, wired to the
/// view.
pub(super) fn controls(window: &mut Window, cx: &mut Context<ReviewView>) -> Controls {
    let mode_state = cx.new(|cx| {
        let mut state = SelectState::new(mode_options(), None, window, cx);
        state.set_selected_value(&ReviewMode::WorkingTree, window, cx);
        state
    });
    let base_state = cx.new(|cx| SelectState::new(Vec::new(), None, window, cx));
    let picker = cx.new(|cx| RepoPicker::new("Which repository to review?", "review", window, cx));
    let subscriptions = vec![
        cx.subscribe_in(
            &mode_state,
            window,
            |this, _, event: &SelectEvent<Vec<ModeOption>>, _, cx| {
                if let SelectEvent::Confirm(Some(mode)) = event {
                    this.set_mode(*mode, cx);
                }
            },
        ),
        cx.subscribe_in(
            &base_state,
            window,
            |this, _, event: &SelectEvent<Vec<BaseOption>>, _, cx| {
                if let SelectEvent::Confirm(Some(branch)) = event {
                    this.set_base(branch.clone(), cx);
                }
            },
        ),
        cx.subscribe(&picker, |this, _, picked: &RepoPicked<PathBuf>, cx| {
            this.picker_open = false;
            this.show_repo(Some(picked.0.clone()), cx);
        }),
    ];
    (mode_state, base_state, picker, subscriptions)
}

/// The rotating double arrow while something is read, else faded and
/// still (queued).
pub(super) fn scan_marker(active: bool, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    match active {
        true => crate::shared::spinner::spinner("icons/arrow_circle.svg")
            .size(px(12.))
            .text_color(muted)
            .into_any_element(),
        false => gpui_kit::svg()
            .size(px(12.))
            .path("icons/arrow_circle.svg")
            .text_color(muted.opacity(0.4))
            .into_any_element(),
    }
}

/// The repository shown at first: the one shown last, else the folder's
/// own (the one all others are inside of), else the only one. `None`
/// leaves the choice open.
pub(super) fn first_repo(repos: &[RepoReviewData], last: Option<&Path>) -> Option<PathBuf> {
    let roots: Vec<&Path> = repos.iter().map(|r| r.repo_root.as_path()).collect();
    last.filter(|last| roots.contains(last))
        .or_else(|| {
            roots
                .iter()
                .find(|root| roots.iter().all(|other| other.starts_with(root)))
                .copied()
        })
        .map(Path::to_path_buf)
}

/// The picker's row of a repository: its changes as the note.
fn picker_repo(data: &RepoReviewData) -> PickerRepo<PathBuf> {
    let note = match (data.scan_state, data.files.len()) {
        (ReviewScanState::Done, 0) => Some("no changes".to_owned()),
        (_, 0) => None,
        (_, 1) => Some("1 file".to_owned()),
        (_, n) => Some(format!("{n} files")),
    };
    PickerRepo {
        key: data.repo_root.clone(),
        name: data.label.clone(),
        place: None,
        branch: data.current_branch.clone(),
        note,
        search: vec![data.repo_root.to_string_lossy().into_owned()],
    }
}

impl ReviewView {
    /// Point the view at a session and request its changed files.
    pub fn set_session(&mut self, session_id: Option<String>, cx: &mut Context<Self>) {
        self.session_id = session_id;
        self.has_listing = false;
        self.listing_generation = GENERATION_UNSEEN;
        self.repos.clear();
        self.repo = None;
        self.base_overrides.clear();
        self.clear_files();
        // A new session lists its own repos; the watcher follows the listing.
        self.watcher = None;
        self.watch_task = None;
        self.mode = self
            .session_id
            .as_ref()
            .and_then(|id| {
                crate::shared::ui_state::read(cx, |store| store.get_review_compare_mode(id))
                    .flatten()
            })
            .map_or(ReviewMode::WorkingTree, |mode| match mode.as_str() {
                "branch_vs_base" => ReviewMode::BranchVsBase,
                "unpushed" => ReviewMode::Unpushed,
                _ => ReviewMode::WorkingTree,
            });
        self.request_listing(cx);
        cx.notify();
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

    /// Drop the shown repository's files: another repository, mode or base.
    fn clear_files(&mut self) {
        self.files.clear();
        self.shown.clear();
        self.in_flight = None;
        self.scope += 1;
        self.list.reset();
    }

    pub(super) fn set_mode(&mut self, mode: ReviewMode, cx: &mut Context<Self>) {
        if mode == self.mode {
            return;
        }
        self.mode = mode;
        // Every diff was read for the old mode; the ticks stay.
        for file in &mut self.files {
            file.read_for = None;
        }
        self.scope += 1;
        self.in_flight = None;
        if let Some(session_id) = &self.session_id {
            let value = match mode {
                ReviewMode::WorkingTree => "working_tree",
                ReviewMode::BranchVsBase => "branch_vs_base",
                ReviewMode::Unpushed => "unpushed",
            };
            crate::shared::ui_state::update(cx, |store| {
                store.set_review_compare_mode(session_id, value.to_owned())
            });
        }
        self.request_listing(cx);
        cx.notify();
    }

    fn set_base(&mut self, branch: String, cx: &mut Context<Self>) {
        let Some(root) = self.repo.clone() else {
            return;
        };
        if self.repo_data().and_then(|d| d.base.as_deref()) == Some(branch.as_str()) {
            return;
        }
        for file in &mut self.files {
            file.read_for = None;
        }
        self.scope += 1;
        self.in_flight = None;
        self.base_overrides.insert(root, branch.clone());
        // Remembered as the default for other repos and sessions.
        self.default_base = Some(branch.clone());
        crate::update_ui_settings(cx, |s| s.review_default_base = Some(branch));
        self.request_listing(cx);
        cx.notify();
    }

    /// Show the repository at `root` (`None`: let the user choose).
    pub(super) fn show_repo(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        if root == self.repo {
            return;
        }
        self.repo = root.clone();
        self.clear_files();
        let store = cx
            .try_global::<Gpui>()
            .map(|gpui| gpui.stores.reviewed_files.clone());
        self.ticks = match (&root, store) {
            (Some(root), Some(store)) => super::Ticks::new(store.load(root).unwrap_or_else(|e| {
                tracing::warn!("Review: viewed files not read: {e:#}");
                Default::default()
            })),
            _ => Default::default(),
        };
        self.picker
            .update(cx, |picker, cx| picker.set_selected(root.clone(), cx));
        if let (Some(session_id), Some(root)) = (&self.session_id, &root) {
            crate::shared::ui_state::update(cx, |store| {
                store.set_review_repo(session_id, root.clone())
            });
        }
        self.sync_repo_files(cx);
        cx.notify();
    }

    /// Consume the latest listing from the global if it changed.
    pub(super) fn sync_listing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((generation, listing)) = cx
            .try_global::<Gpui>()
            .and_then(|g| g.review_listing_if_newer(self.listing_generation))
        else {
            return;
        };
        self.listing_generation = generation;
        match listing {
            Some(listing) => self.take_listing(listing, window, cx),
            None => {
                self.has_listing = false;
                self.repos.clear();
            }
        }
    }

    /// Take a listing of the session's repositories.
    pub(super) fn take_listing(
        &mut self,
        listing: crate::ReviewData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A listing for another mode is on its way out.
        if listing.mode != self.mode {
            return;
        }
        self.has_listing = true;
        self.is_git_repo = listing.is_git_repo;
        self.repos = listing.repos;
        self.mode_state.update(cx, |state, cx| {
            state.set_selected_value(&listing.mode, window, cx);
        });
        let rows = self.repos.iter().map(picker_repo).collect();
        self.picker
            .update(cx, |picker, cx| picker.set_repos(rows, cx));

        if self
            .repo
            .as_ref()
            .is_none_or(|root| !self.repos.iter().any(|r| &r.repo_root == root))
        {
            let last = self.session_id.as_ref().and_then(|id| {
                crate::shared::ui_state::read(cx, |store| store.get_review_repo(id)).flatten()
            });
            let first = first_repo(&self.repos, last.as_deref());
            self.repo = None;
            self.show_repo(first, cx);
        } else {
            self.sync_repo_files(cx);
        }
        self.sync_base_select(window, cx);
        self.seed_default_base(cx);
        self.ensure_watcher(cx);
    }

    /// The base selector follows the shown repository's candidates.
    fn sync_base_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(data) = self.repo_data() else {
            return;
        };
        let items: Vec<BaseOption> = data
            .base_candidates
            .iter()
            .map(|b| BaseOption { branch: b.clone() })
            .collect();
        let base = data.base.clone();
        self.base_state.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            if let Some(base) = &base {
                state.set_selected_value(base, window, cx);
            }
        });
    }

    /// Apply the remembered default base to repos without a choice of
    /// their own. Ends once the listing echoes the default as their base.
    fn seed_default_base(&mut self, cx: &mut Context<Self>) {
        let Some(default) = self
            .default_base
            .clone()
            .filter(|_| self.mode == ReviewMode::BranchVsBase)
        else {
            return;
        };
        let mut seeded = false;
        for data in &self.repos {
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

    /// Take the shown repository's listed files: known ones keep their
    /// diffs (stale ones until read again), new ones show a header, viewed
    /// ones come folded.
    pub(super) fn sync_repo_files(&mut self, cx: &mut Context<Self>) {
        let listed = self
            .repo_data()
            .map(|data| data.files.clone())
            .unwrap_or_default();
        let unchanged = listed.len() == self.files.len()
            && listed.iter().zip(&self.files).all(|(l, f)| *l == f.listed);
        if unchanged {
            return;
        }
        let mut old: Vec<ReviewFile> = std::mem::take(&mut self.files);
        let mut files = Vec::with_capacity(listed.len());
        for entry in listed {
            let file = match old.iter().position(|f| f.listed.path == entry.path) {
                Some(ix) => {
                    let mut file = old.swap_remove(ix);
                    if file.read_for.is_none() && file.listed != entry {
                        file.file = self.pending_file(&entry);
                    }
                    file.listed = entry;
                    file
                }
                None => {
                    let file = ReviewFile {
                        file: self.pending_file(&entry),
                        listed: entry,
                        read_for: None,
                        content: None,
                    };
                    if self.tick_of(&file).mark().viewed {
                        self.list.folds.set(&file.listed.path, true);
                    }
                    file
                }
            };
            files.push(file);
        }
        self.files = files;
        if let Some((path, _)) = &self.in_flight
            && self.review_file(path).is_none_or(|f| f.current())
        {
            self.in_flight = None;
        }
        self.list.dirty = true;
        self.sync_files(cx);
    }

    fn pending_file(&mut self, entry: &git::ChangedFile) -> LoadedFile {
        let mut file =
            LoadedFile::pending(entry.path.clone(), entry.orig_path.clone(), entry.status);
        file.stamp = self.list.stamp();
        file
    }

    /// Keep a change watcher running on exactly the listed repos; its
    /// callbacks ask for a new listing.
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
        // thread is busy; dropping the watcher closes it, ending the task.
        let (tx, rx) = async_channel::bounded::<()>(1);
        match git::ChangeWatcher::start(&roots, WATCH_DEBOUNCE, move || {
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

    /// The shown repository, and the popover to choose another (when there
    /// is a choice).
    pub(super) fn render_repo_trigger(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let data = self.repo_data()?;
        if !self.picker.read(cx).offers_a_choice() {
            return None;
        }
        let this = cx.entity().downgrade();
        let picker = self.picker.clone();
        Some(
            div()
                .flex_none()
                .debug_selector(|| "review-repo-picker".into())
                .child(
                    Popover::new("review-repo-picker")
                        .anchor(gpui_kit::Anchor::TopLeft)
                        .open(self.picker_open)
                        .on_open_change(move |open, window, cx| {
                            let open = *open;
                            this.update(cx, |this, cx| {
                                this.picker_open = open;
                                if open {
                                    this.picker
                                        .update(cx, |picker, cx| picker.start_search(window, cx));
                                }
                                cx.notify();
                            })
                            .ok();
                        })
                        .trigger(
                            Button::new("review-repo-picker-trigger")
                                .outline()
                                .xsmall()
                                .label(data.label.clone())
                                .dropdown_caret(true)
                                .tooltip(data.repo_root.to_string_lossy().into_owned()),
                        )
                        .content(move |_, _, _| picker.clone().into_any_element()),
                )
                .into_any_element(),
        )
    }

    pub(super) fn render_mode_select(&self, cx: &App) -> AnyElement {
        div()
            .flex_none()
            .child(
                Select::new(&self.mode_state)
                    .placeholder("Compare")
                    .with_size(Size::XSmall)
                    .icon(
                        Icon::default()
                            .path("icons/chevron_up_down.svg")
                            .with_size(Size::XSmall)
                            .text_color(cx.theme().muted_foreground),
                    )
                    .min_w(px(130.)),
            )
            .into_any_element()
    }

    /// What the mode compares with: the base to choose (branch mode), the
    /// upstream (unpushed); a spinner while the repository is scanned.
    pub(super) fn render_base(&self, cx: &App) -> AnyElement {
        let Some(data) = self.repo_data() else {
            return div().into_any_element();
        };
        let scanning = match data.scan_state {
            ReviewScanState::Done => None,
            state => Some(scan_marker(state == ReviewScanState::Scanning, cx)),
        };
        let slot = match self.mode {
            ReviewMode::WorkingTree => div().into_any_element(),
            ReviewMode::BranchVsBase => div()
                .min_w_0()
                .child(
                    Select::new(&self.base_state)
                        .placeholder("Base")
                        .with_size(Size::XSmall)
                        .icon(
                            Icon::default()
                                .path("icons/chevron_up_down.svg")
                                .with_size(Size::XSmall)
                                .text_color(cx.theme().muted_foreground),
                        )
                        .min_w(px(120.)),
                )
                .into_any_element(),
            ReviewMode::Unpushed => {
                let text = match &data.base {
                    Some(upstream) if data.files.is_empty() => {
                        format!("Up to date with {upstream}")
                    }
                    Some(upstream) => format!("Not yet in {upstream}"),
                    None if data.scan_state == ReviewScanState::Done => {
                        "No upstream branch to compare with".to_owned()
                    }
                    None => "Looking for the upstream branch…".to_owned(),
                };
                div().min_w_0().truncate().child(text).into_any_element()
            }
        };
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .child(slot)
            .children(scanning)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(root: &str) -> RepoReviewData {
        RepoReviewData {
            repo_root: PathBuf::from(root),
            label: root.rsplit('/').next().unwrap_or(root).to_owned(),
            current_branch: None,
            base_candidates: Vec::new(),
            base: None,
            files: Vec::new(),
            stats: git::DiffStats::default(),
            scan_state: ReviewScanState::Done,
        }
    }

    #[test]
    fn the_folders_own_repository_or_the_last_one_shows_first() {
        let nested = [repo("/work"), repo("/work/vendor/lib")];
        assert_eq!(first_repo(&nested, None), Some(PathBuf::from("/work")));
        assert_eq!(
            first_repo(&nested, Some(Path::new("/work/vendor/lib"))),
            Some(PathBuf::from("/work/vendor/lib"))
        );
        let side_by_side = [repo("/work/a"), repo("/work/b")];
        assert_eq!(first_repo(&side_by_side, None), None);
        assert_eq!(first_repo(&side_by_side, Some(Path::new("/gone"))), None);
        assert_eq!(
            first_repo(&[repo("/work/a")], None),
            Some(PathBuf::from("/work/a"))
        );
    }
}
