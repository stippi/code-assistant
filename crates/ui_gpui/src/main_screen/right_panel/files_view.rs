//! The "Files" view of the right panel: the session's project as a lazy
//! tree on the left, the selected file on the right ([`FileViewer`]).
//!
//! Directories load one level at a time through the `SessionService` when
//! expanded. A filter field above the tree switches it to a flat list of
//! fuzzy matches over every file gitignore keeps. A [`TreeWatcher`] on the
//! project root re-lists loaded directories and reloads the open file when
//! they change on disk. The open file, the expanded directories and whether
//! ignored entries show are remembered per session.

use super::file_filter::{FileMatch, filter_paths};
use super::file_viewer::FileViewer;
use crate::Gpui;
use crate::comments::CommentChange;
use crate::shared::{file_icons, ui_state};
use code_assistant_core::line_comments::LineComment;
use code_assistant_core::session::{DirListing, EntryKind, TreeWatcher};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size};
use gpui_kit::{
    Context, Entity, FocusHandle, Focusable, MouseButton, MouseMoveEvent, Pixels, Render,
    SharedString, StyledText, Subscription, Task, UniformListScrollHandle, Window, div, prelude::*,
    px, uniform_list,
};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Filter results shown at most.
const MAX_MATCHES: usize = 200;
/// Quiet period before the watcher reports a burst of changes.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(300);
const DEFAULT_TREE_WIDTH: Pixels = px(170.);
const MIN_TREE_WIDTH: Pixels = px(100.);
const MIN_VIEWER_WIDTH: Pixels = px(160.);

/// A directory's listing state, keyed by its `/`-separated relative path
/// (`""` for the root).
enum DirState {
    Loading,
    Loaded(DirListing),
    Failed(String),
}

/// One row of the tree.
#[derive(Clone)]
enum TreeRow {
    Entry {
        path: String,
        name: SharedString,
        depth: usize,
        kind: EntryKind,
        ignored: bool,
        expanded: bool,
    },
    /// A loading, failure or "N more" note under a directory.
    Note { depth: usize, text: SharedString },
}

/// Join a directory and an entry name into a relative path.
fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

/// The parent directory of a relative path (`""` for top-level entries).
fn parent(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

pub struct FilesView {
    session_id: Option<String>,
    /// The project root, once the first listing named it.
    root: Option<PathBuf>,
    dirs: HashMap<String, DirState>,
    expanded: BTreeSet<String>,
    show_ignored: bool,
    rows: Vec<TreeRow>,
    tree_scroll: UniformListScrollHandle,
    viewer: Entity<FileViewer>,

    tree_visible: bool,
    tree_width: Pixels,
    /// Pointer x and tree width when a divider drag began.
    resizing: Option<(Pixels, Pixels)>,

    filter: Entity<InputState>,
    query: String,
    /// Every file gitignore keeps; loaded when the filter is first used and
    /// dropped when the tree changes.
    all_files: Option<Arc<Vec<String>>>,
    files_task: Option<Task<()>>,
    matches: Vec<FileMatch>,
    /// The filter field still shows the previous session's text.
    clear_filter: bool,
    /// A comment to reveal once the project root is known.
    pending_reveal: Option<LineComment>,
    /// An absolute path (and line) to open once the project root is known.
    pending_open: Option<(String, Option<usize>)>,

    watcher: Option<(PathBuf, TreeWatcher)>,
    watch_task: Option<Task<()>>,
    load_tasks: HashMap<String, Task<()>>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl FilesView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let viewer = cx.new(FileViewer::new);
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter files"));
        let subscriptions = vec![
            cx.subscribe_in(&filter, window, Self::on_filter_event),
            // The header shows the viewer's state.
            cx.observe(&viewer, |_, _, cx| cx.notify()),
            cx.subscribe(&viewer, |_, _, change: &CommentChange, cx| {
                cx.emit(change.clone())
            }),
        ];
        Self {
            session_id: None,
            root: None,
            dirs: HashMap::new(),
            expanded: BTreeSet::new(),
            show_ignored: false,
            rows: Vec::new(),
            tree_scroll: UniformListScrollHandle::new(),
            viewer,
            tree_visible: true,
            tree_width: DEFAULT_TREE_WIDTH,
            resizing: None,
            filter,
            query: String::new(),
            all_files: None,
            files_task: None,
            matches: Vec::new(),
            clear_filter: false,
            pending_reveal: None,
            pending_open: None,
            watcher: None,
            watch_task: None,
            load_tasks: HashMap::new(),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    #[cfg(test)]
    pub fn viewer(&self) -> &Entity<FileViewer> {
        &self.viewer
    }

    pub fn set_comments(&mut self, comments: Vec<LineComment>, cx: &mut Context<Self>) {
        self.viewer
            .update(cx, |viewer, cx| viewer.set_comments(comments, cx));
    }

    /// Open `path`, relative to the project root or absolute inside it, and
    /// select `line` (1-based). An absolute path waits for the root.
    pub fn open_path(&mut self, path: String, line: Option<usize>, cx: &mut Context<Self>) {
        let rel = if std::path::Path::new(&path).is_absolute() {
            let Some(root) = self.root.clone() else {
                self.pending_open = Some((path, line));
                return;
            };
            match std::path::Path::new(&path).strip_prefix(&root) {
                Ok(rel) => rel
                    .iter()
                    .map(|c| c.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/"),
                Err(_) => return,
            }
        } else {
            path.trim_start_matches("./").to_owned()
        };
        self.open_file(rel, cx);
        if let Some(line) = line {
            self.viewer
                .update(cx, |viewer, cx| viewer.reveal_line(line, cx));
        }
    }

    /// Open the file of `comment` and its lines. Waits for the project root
    /// when the view was just attached.
    pub fn reveal_comment(&mut self, comment: LineComment, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            self.pending_reveal = Some(comment);
            return;
        };
        let Ok(rel) = comment.file.strip_prefix(&root) else {
            return;
        };
        let path = rel
            .iter()
            .map(|c| c.to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        self.open_file(path, cx);
        self.viewer
            .update(cx, |viewer, cx| viewer.reveal(comment, cx));
    }

    /// Show `session_id`'s project; `None` detaches the view (no watcher).
    pub fn set_session(&mut self, session_id: Option<String>, cx: &mut Context<Self>) {
        if self.session_id == session_id {
            return;
        }
        self.session_id = session_id.clone();
        self.root = None;
        self.dirs.clear();
        self.load_tasks.clear();
        self.all_files = None;
        self.files_task = None;
        self.matches.clear();
        self.query.clear();
        self.watcher = None;
        self.watch_task = None;
        // Emptying the field needs a window; render does it.
        self.clear_filter = true;

        let saved = session_id
            .as_deref()
            .and_then(|id| ui_state::read(cx, |store| store.get_files_view(id)))
            .unwrap_or_default();
        self.expanded = saved.expanded.into_iter().collect();
        self.show_ignored = saved.show_ignored;
        let session_for_viewer = session_id.clone();
        self.viewer.update(cx, |viewer, cx| {
            viewer.open(session_for_viewer, saved.open_file, cx)
        });
        if session_id.is_some() {
            self.load_dir(String::new(), cx);
            for dir in self.expanded.clone() {
                self.load_dir(dir, cx);
            }
        }
        self.rebuild_rows();
        cx.notify();
    }

    /// Re-list the loaded directories and reload the open file.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let loaded: Vec<String> = self.dirs.keys().cloned().collect();
        for dir in loaded {
            self.load_dir(dir, cx);
        }
        self.all_files = None;
        self.refresh_matches(cx);
        self.viewer.update(cx, |viewer, cx| viewer.reload(cx));
    }

    /// Open `path` (relative, `/`-separated) in the viewer and reveal it in
    /// the tree.
    pub fn open_file(&mut self, path: String, cx: &mut Context<Self>) {
        // Expand the ancestors so the file shows in the tree.
        let mut dir = parent(&path).to_owned();
        while !dir.is_empty() {
            if self.expanded.insert(dir.clone()) && !self.dirs.contains_key(&dir) {
                self.load_dir(dir.clone(), cx);
            }
            dir = parent(&dir).to_owned();
        }
        let session_id = self.session_id.clone();
        self.viewer.update(cx, |viewer, cx| {
            viewer.open(session_id, Some(path.clone()), cx)
        });
        self.rebuild_rows();
        if let Some(ix) = self
            .rows
            .iter()
            .position(|row| matches!(row, TreeRow::Entry { path: p, .. } if *p == path))
        {
            self.tree_scroll
                .scroll_to_item(ix, gpui_kit::ScrollStrategy::Center);
        }
        self.persist(cx);
        cx.notify();
    }

    fn persist(&self, cx: &mut Context<Self>) {
        let Some(session_id) = &self.session_id else {
            return;
        };
        let state = ui_state::FilesViewState {
            open_file: self.viewer.read(cx).path().map(str::to_owned),
            expanded: self.expanded.iter().cloned().collect(),
            show_ignored: self.show_ignored,
        };
        ui_state::update(cx, |store| store.set_files_view(session_id, state));
    }

    fn load_dir(&mut self, dir: String, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let Some(service) = cx
            .try_global::<Gpui>()
            .and_then(|gpui| gpui.session_service())
        else {
            return;
        };
        // Keep showing the old listing while a re-list runs.
        self.dirs.entry(dir.clone()).or_insert(DirState::Loading);
        let key = dir.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = service
                .list_project_dir(session_id.clone(), PathBuf::from(&dir))
                .await;
            this.update(cx, |this, cx| {
                if this.session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }
                this.load_tasks.remove(&dir);
                match result {
                    Ok(listed) => {
                        if this.root.as_ref() != Some(&listed.root) {
                            this.set_root(listed.root, &dir, cx);
                        }
                        this.dirs.insert(dir, DirState::Loaded(listed.listing));
                    }
                    Err(e) => {
                        // A directory that vanished collapses quietly.
                        if dir.is_empty() {
                            this.dirs.insert(dir, DirState::Failed(format!("{e:#}")));
                        } else {
                            this.dirs.remove(&dir);
                            this.expanded.remove(&dir);
                        }
                    }
                }
                this.rebuild_rows();
                cx.notify();
            })
            .ok();
        });
        self.load_tasks.insert(key, task);
    }

    /// The project root became known or moved (a worktree switch): drop
    /// listings of the old root and watch the new one.
    fn set_root(&mut self, root: PathBuf, loaded_dir: &str, cx: &mut Context<Self>) {
        let moved = self.root.is_some();
        self.root = Some(root.clone());
        if moved {
            self.dirs.retain(|dir, _| dir == loaded_dir);
            self.all_files = None;
            for dir in std::iter::once(String::new()).chain(self.expanded.clone()) {
                if dir != loaded_dir {
                    self.load_dir(dir, cx);
                }
            }
            self.viewer.update(cx, |viewer, cx| viewer.reload(cx));
        }
        self.start_watcher(root, cx);
        if let Some(comment) = self.pending_reveal.take() {
            self.reveal_comment(comment, cx);
        }
        if let Some((path, line)) = self.pending_open.take() {
            self.open_path(path, line, cx);
        }
    }

    fn start_watcher(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.watcher.as_ref().is_some_and(|(r, _)| *r == root) {
            return;
        }
        let (tx, rx) = async_channel::unbounded::<BTreeSet<PathBuf>>();
        match TreeWatcher::start(&root, WATCH_DEBOUNCE, move |changed| {
            let _ = tx.send_blocking(changed);
        }) {
            Ok(watcher) => {
                self.watcher = Some((root, watcher));
                self.watch_task = Some(cx.spawn(async move |this, cx| {
                    while let Ok(changed) = rx.recv().await {
                        if this
                            .update(cx, |this, cx| this.on_changed(changed, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                }));
            }
            Err(e) => {
                tracing::warn!("Files view: watcher unavailable: {e:#}");
                self.watcher = None;
                self.watch_task = None;
            }
        }
    }

    /// Re-list loaded directories whose entries changed and reload the open
    /// file when it changed.
    fn on_changed(&mut self, changed: BTreeSet<PathBuf>, cx: &mut Context<Self>) {
        let changed: Vec<String> = changed
            .iter()
            .map(|p| {
                p.iter()
                    .map(|c| c.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .collect();
        let mut relist = BTreeSet::new();
        for path in &changed {
            let dir = parent(path);
            if self.dirs.contains_key(dir) {
                relist.insert(dir.to_owned());
            }
            if self.dirs.contains_key(path.as_str()) {
                relist.insert(path.clone());
            }
        }
        // Content edits change no listing, but a file appearing or vanishing
        // may change the filter's file list.
        if !relist.is_empty() {
            self.all_files = None;
            self.refresh_matches(cx);
        }
        for dir in relist {
            self.load_dir(dir, cx);
        }
        let open = self.viewer.read(cx).path().map(str::to_owned);
        if open.is_some_and(|open| changed.contains(&open)) {
            self.viewer.update(cx, |viewer, cx| viewer.reload(cx));
        }
    }

    fn toggle_dir(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.expanded.remove(&path) {
            self.expanded.insert(path.clone());
            if !self.dirs.contains_key(&path) {
                self.load_dir(path, cx);
            }
        }
        self.rebuild_rows();
        self.persist(cx);
        cx.notify();
    }

    fn toggle_ignored(&mut self, cx: &mut Context<Self>) {
        self.show_ignored = !self.show_ignored;
        self.rebuild_rows();
        self.persist(cx);
        cx.notify();
    }

    /// Flatten the expanded part of the tree into rows.
    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        self.push_rows("", 0, &mut rows);
        self.rows = rows;
    }

    fn push_rows(&self, dir: &str, depth: usize, rows: &mut Vec<TreeRow>) {
        match self.dirs.get(dir) {
            None | Some(DirState::Loading) => rows.push(TreeRow::Note {
                depth,
                text: "Loading…".into(),
            }),
            Some(DirState::Failed(error)) => rows.push(TreeRow::Note {
                depth,
                text: error.clone().into(),
            }),
            Some(DirState::Loaded(listing)) => {
                for entry in &listing.entries {
                    if entry.ignored && !self.show_ignored {
                        continue;
                    }
                    let path = join(dir, &entry.name);
                    let expanded = entry.kind == EntryKind::Dir && self.expanded.contains(&path);
                    rows.push(TreeRow::Entry {
                        path: path.clone(),
                        name: entry.name.clone().into(),
                        depth,
                        kind: entry.kind,
                        ignored: entry.ignored,
                        expanded,
                    });
                    if expanded {
                        self.push_rows(&path, depth + 1, rows);
                    }
                }
                if listing.omitted > 0 {
                    rows.push(TreeRow::Note {
                        depth,
                        text: format!("{} more not shown", listing.omitted).into(),
                    });
                }
            }
        }
    }

    fn on_filter_event(
        &mut self,
        _input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let query = self.filter.read(cx).value().trim().to_owned();
                if query != self.query {
                    self.query = query;
                    self.refresh_matches(cx);
                }
            }
            InputEvent::PressEnter { .. } => {
                if let Some(first) = self.matches.first() {
                    let path = first.path.clone();
                    self.open_file(path, cx);
                }
            }
            _ => {}
        }
    }

    /// Recompute the filter matches, loading the file list first when needed.
    fn refresh_matches(&mut self, cx: &mut Context<Self>) {
        if self.query.is_empty() {
            self.matches.clear();
            cx.notify();
            return;
        }
        match &self.all_files {
            Some(files) => {
                self.matches = filter_paths(files, &self.query, MAX_MATCHES);
                cx.notify();
            }
            None => self.load_all_files(cx),
        }
    }

    fn load_all_files(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let Some(service) = cx
            .try_global::<Gpui>()
            .and_then(|gpui| gpui.session_service())
        else {
            return;
        };
        self.files_task = Some(cx.spawn(async move |this, cx| {
            let result = service.list_project_files(session_id.clone()).await;
            this.update(cx, |this, cx| {
                if this.session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }
                match result {
                    Ok(files) => {
                        this.all_files = Some(Arc::new(files.files));
                        this.refresh_matches(cx);
                    }
                    Err(e) => tracing::warn!("Files view: cannot list files: {e:#}"),
                }
            })
            .ok();
        }));
    }

    // -----------------------------------------------------------------------
    // Rendering
    // -----------------------------------------------------------------------

    fn render_tree_row(
        &self,
        row: &TreeRow,
        ix: usize,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        let theme = cx.theme();
        let indent = |depth: usize| px(8. + depth as f32 * 12.);
        match row {
            TreeRow::Note { depth, text } => div()
                .h(px(22.))
                .flex()
                .items_center()
                .pl(indent(*depth) + px(14.))
                .text_xs()
                .text_color(theme.muted_foreground)
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(text.clone())
                .into_any_element(),
            TreeRow::Entry {
                path,
                name,
                depth,
                kind,
                ignored,
                expanded,
            } => {
                let is_dir = *kind == EntryKind::Dir;
                let selected = !is_dir && self.viewer.read(cx).path() == Some(path.as_str());
                let icon = if is_dir {
                    file_icons::get().get_type_icon(if *expanded {
                        "expanded_folder"
                    } else {
                        "collapsed_folder"
                    })
                } else {
                    file_icons::get().get_icon_for_filename(name)
                };
                let chevron = is_dir.then(|| {
                    gpui_kit::svg()
                        .size(px(10.))
                        .path(if *expanded {
                            "icons/chevron_down.svg"
                        } else {
                            "icons/chevron_right.svg"
                        })
                        .text_color(theme.muted_foreground)
                });
                let fg = if *ignored {
                    theme.muted_foreground
                } else {
                    theme.foreground
                };
                let path = path.clone();
                div()
                    .id(("files-tree-row", ix))
                    .h(px(22.))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_1()
                    .pl(indent(*depth))
                    .pr_2()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(theme.list_active))
                    .hover(|s| s.bg(theme.list_hover))
                    .child(div().w(px(10.)).flex_none().children(chevron))
                    .child(file_icons::render_icon(
                        &icon,
                        14.0,
                        theme.muted_foreground,
                        "·",
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_xs()
                            .text_color(fg)
                            .child(name.clone()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if is_dir {
                            this.toggle_dir(path.clone(), cx);
                        } else {
                            this.open_file(path.clone(), cx);
                        }
                    }))
                    .into_any_element()
            }
        }
    }

    fn render_match(&self, m: &FileMatch, ix: usize, cx: &Context<Self>) -> gpui_kit::AnyElement {
        let theme = cx.theme();
        let selected = self.viewer.read(cx).path() == Some(m.path.as_str());
        let name_start = m.path.rfind('/').map_or(0, |i| i + 1);
        let name = &m.path[name_start..];
        let dir = &m.path[..name_start.saturating_sub(1)];
        let highlight = gpui_kit::HighlightStyle {
            color: Some(theme.primary),
            font_weight: Some(gpui_kit::FontWeight::BOLD),
            ..Default::default()
        };
        let name_highlights: Vec<_> = m
            .positions
            .iter()
            .filter(|&&p| p >= name_start)
            .map(|&p| {
                let len = m.path[p..].chars().next().map_or(1, char::len_utf8);
                (p - name_start..p - name_start + len, highlight)
            })
            .collect();
        let icon = file_icons::get().get_icon_for_filename(name);
        let path = m.path.clone();
        div()
            .id(("files-match-row", ix))
            .h(px(22.))
            .w_full()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .cursor_pointer()
            .when(selected, |d| d.bg(theme.list_active))
            .hover(|s| s.bg(theme.list_hover))
            .child(file_icons::render_icon(
                &icon,
                14.0,
                theme.muted_foreground,
                "·",
            ))
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.foreground)
                    .child(
                        StyledText::new(SharedString::from(name.to_owned()))
                            .with_highlights(name_highlights),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(dir.to_owned()),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.open_file(path.clone(), cx)))
            .into_any_element()
    }

    fn render_tree(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let filtering = !self.query.is_empty();
        let count = if filtering {
            self.matches.len()
        } else {
            self.rows.len()
        };
        if filtering && count == 0 {
            let text = if self.all_files.is_none() {
                "Searching…"
            } else {
                "No matching files"
            };
            return div()
                .p_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(text)
                .into_any_element();
        }
        div()
            .size_full()
            .child(
                uniform_list(
                    "files-tree",
                    count,
                    cx.processor(
                        move |this: &mut Self, range: std::ops::Range<usize>, _, cx| {
                            range
                                .filter_map(|ix| {
                                    if filtering {
                                        this.matches.get(ix).map(|m| this.render_match(m, ix, cx))
                                    } else {
                                        this.rows
                                            .get(ix)
                                            .map(|row| this.render_tree_row(row, ix, cx))
                                    }
                                })
                                .collect::<Vec<_>>()
                        },
                    ),
                )
                .track_scroll(&self.tree_scroll)
                .size_full(),
            )
            .vertical_scrollbar(&self.tree_scroll)
            .into_any_element()
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let tree_visible = self.tree_visible;
        let show_ignored = self.show_ignored;
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("files-toggle-tree")
                    .icon(
                        Icon::default()
                            .path(SharedString::from(if tree_visible {
                                "icons/panel_left_close.svg"
                            } else {
                                "icons/panel_left_open.svg"
                            }))
                            .with_size(Size::XSmall),
                    )
                    .ghost()
                    .xsmall()
                    .tooltip(if tree_visible {
                        "Hide file tree"
                    } else {
                        "Show file tree"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.tree_visible = !this.tree_visible;
                        cx.notify();
                    })),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&self.filter)
                        .with_size(Size::XSmall)
                        .prefix(
                            Icon::default()
                                .path(SharedString::from("icons/magnifying_glass.svg"))
                                .with_size(Size::XSmall)
                                .text_color(theme.muted_foreground),
                        )
                        .cleanable(true),
                ),
            )
            .child(
                Button::new("files-toggle-ignored")
                    .label(".gitignore")
                    .xsmall()
                    .map(|b| if show_ignored { b.primary() } else { b.ghost() })
                    .tooltip(if show_ignored {
                        "Hide gitignored files"
                    } else {
                        "Show gitignored files"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_ignored(cx))),
            )
    }

    /// Path of the open file and the actions on its selection.
    fn render_viewer_header(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let theme = cx.theme();
        let viewer = self.viewer.read(cx);
        let path = viewer.path()?.to_owned();
        let dirty = viewer.is_dirty();
        Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .py_0p5()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(path),
                )
                .when(dirty, |d| {
                    d.child(
                        div()
                            .size(px(7.))
                            .rounded_full()
                            .bg(theme.muted_foreground)
                            .id("files-unsaved")
                            .tooltip(|window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new("Unsaved edits")
                                    .build(window, cx)
                            }),
                    )
                }),
        )
    }

    fn on_resize_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((start_x, start_width)) = self.resizing else {
            return;
        };
        if !event.dragging() {
            self.resizing = None;
            cx.notify();
            return;
        }
        let max = (window.bounds().size.width - MIN_VIEWER_WIDTH).max(MIN_TREE_WIDTH);
        self.tree_width = (start_width + event.position.x - start_x).clamp(MIN_TREE_WIDTH, max);
        cx.notify();
    }
}

impl gpui_kit::EventEmitter<CommentChange> for FilesView {}

impl Focusable for FilesView {
    fn focus_handle(&self, _cx: &gpui_kit::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for FilesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.clear_filter) {
            self.filter
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        let theme = cx.theme();
        let border = theme.border;
        let handle_color = if self.resizing.is_some() {
            theme.drag_border
        } else {
            theme.border
        };

        if let Some(DirState::Failed(error)) = self.dirs.get("") {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(error.clone())
                .into_any_element();
        }

        let tree = self.tree_visible.then(|| {
            div()
                .flex_none()
                .h_full()
                .w(self.tree_width)
                .child(self.render_tree(cx))
        });
        let divider = self.tree_visible.then(|| {
            div()
                .id("files-divider")
                .flex_none()
                .h_full()
                .w(px(5.))
                .cursor_col_resize()
                .child(div().mx_auto().h_full().w(px(1.)).bg(handle_color))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &gpui_kit::MouseDownEvent, _, cx| {
                        this.resizing = Some((event.position.x, this.tree_width));
                        cx.notify();
                    }),
                )
        });
        let viewer_column = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .when(!self.tree_visible, |d| d.border_l_0())
            .children(self.render_viewer_header(cx))
            .child(div().flex_1().min_h_0().child(self.viewer.clone()));

        div()
            .size_full()
            .flex()
            .flex_col()
            .track_focus(&self.focus_handle)
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .border_color(border)
                    .on_mouse_move(cx.listener(Self::on_resize_move))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            if this.resizing.take().is_some() {
                                cx.notify();
                            }
                        }),
                    )
                    .children(tree)
                    .children(divider)
                    .child(viewer_column),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_join_and_split() {
        assert_eq!(join("", "a"), "a");
        assert_eq!(join("a/b", "c"), "a/b/c");
        assert_eq!(parent("a/b/c"), "a/b");
        assert_eq!(parent("a"), "");
    }
}
