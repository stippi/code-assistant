//! The "Files" view of the right panel: the session's project as a lazy
//! tree on the left, the selected file on the right ([`FileViewer`]).
//!
//! Directories load one level at a time through the `SessionService` when
//! expanded. A filter field above the tree switches it to a flat list of
//! fuzzy matches over every file gitignore keeps. A [`TreeWatcher`] on the
//! project root re-lists loaded directories and reloads the open file when
//! they change on disk. The open file, the expanded directories and whether
//! ignored entries show are remembered per session.
//!
//! A right click on an entry opens its menu: copy the path, comment on the
//! whole file (a card under the viewer's path header), delete the file.
//!
//! When the open file can be shown in more ways than as text (rendered
//! Markdown, see [`file_renderers`](super::file_renderers)), a dropdown in
//! the viewer's header picks one. The pick holds for files of the same
//! kind and is remembered per session.

use super::file_filter::{FileMatch, filter_paths};
use super::file_renderers;
use super::file_viewer::FileViewer;
use crate::Gpui;
use crate::comments::editor::{CommentEditor, CommentEditorEvent};
use crate::comments::{AnchorTracker, CommentChange, floating};
use crate::shared::{file_icons, ui_state};
use code_assistant_core::line_comments::LineComment;
use code_assistant_core::session::{DirListing, EntryKind, TreeWatcher};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size};
use gpui_kit::{
    Bounds, ClipboardItem, Context, Entity, FocusHandle, Focusable, MouseButton, MouseDownEvent,
    MouseMoveEvent, Pixels, Point, Render, SharedString, StyledText, Subscription, Task,
    UniformListScrollHandle, Window, canvas, div, point, prelude::*, px, uniform_list,
};
use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
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

/// A choice in the renderer dropdown.
#[derive(Clone)]
struct RendererItem {
    id: String,
    label: SharedString,
}

impl SelectItem for RendererItem {
    type Value = String;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }
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

/// The menu of a tree entry, opened by a right click.
struct EntryMenu {
    path: String,
    is_dir: bool,
    /// Where it was opened, in window coordinates.
    position: Point<Pixels>,
    /// Asking whether to delete the file.
    confirm_delete: bool,
    /// Why deleting failed.
    error: Option<String>,
}

/// Join a directory and an entry name into a relative path.
fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

/// The absolute path of a relative path below `root`. Joined per
/// component, so it uses the platform's separators.
fn absolute_path(root: &Path, path: &str) -> PathBuf {
    path.split('/')
        .fold(root.to_path_buf(), |file, part| file.join(part))
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
    renderer_select: Entity<SelectState<Vec<RendererItem>>>,
    /// The renderer ids the dropdown offers, and the selected one.
    renderer_shown: (Vec<String>, String),
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

    menu: Option<EntryMenu>,
    delete_task: Option<Task<()>>,
    /// The draft's comments, to find an existing comment on a whole file.
    comments: Vec<LineComment>,
    /// The card for a comment on the whole open file, under its path header.
    file_comment: Option<Entity<CommentEditor>>,
    /// A comment on the whole open file to open the card for (needs a window).
    pending_file_comment: Option<LineComment>,
    /// Where the path header was last drawn; the card floats below it.
    header_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    file_comment_anchor: AnchorTracker,
    _file_comment_subscription: Option<Subscription>,

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
        let renderer_select =
            cx.new(|cx| SelectState::new(Vec::<RendererItem>::new(), None, window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&filter, window, Self::on_filter_event),
            cx.subscribe_in(&renderer_select, window, Self::on_renderer_selected),
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
            renderer_select,
            renderer_shown: (Vec::new(), file_renderers::TEXT.to_owned()),
            all_files: None,
            files_task: None,
            matches: Vec::new(),
            clear_filter: false,
            pending_reveal: None,
            pending_open: None,
            menu: None,
            delete_task: None,
            comments: Vec::new(),
            file_comment: None,
            pending_file_comment: None,
            header_bounds: Rc::new(Cell::new(None)),
            file_comment_anchor: AnchorTracker::default(),
            _file_comment_subscription: None,
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
        self.comments = comments.clone();
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
        if comment.on_file {
            self.pending_file_comment = Some(comment);
            cx.notify();
            return;
        }
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
        self.menu = None;
        self.delete_task = None;
        self.pending_file_comment = None;
        self.close_file_comment(cx);
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
            viewer.set_renderer_picks(saved.renderers, cx);
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
        if self.viewer.read(cx).path() != Some(path.as_str()) {
            self.close_file_comment(cx);
        }
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

    /// Open `path` and the card for a comment on the whole file (the
    /// existing one, if any).
    fn start_file_comment(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(root) = &self.root else {
            return;
        };
        let file = absolute_path(root, &path);
        let comment = self
            .comments
            .iter()
            .find(|c| c.on_file && c.file == file)
            .cloned()
            .unwrap_or_else(|| LineComment::on_file(file));
        self.open_file(path, cx);
        self.pending_file_comment = Some(comment);
        cx.notify();
    }

    fn open_file_comment(
        &mut self,
        comment: LineComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let card = cx.new(|cx| CommentEditor::new(comment, window, cx));
        self._file_comment_subscription = Some(cx.subscribe(
            &card,
            |this, _, event: &CommentEditorEvent, cx| {
                match event {
                    CommentEditorEvent::Save(comment) => {
                        cx.emit(CommentChange::Upsert(comment.clone()))
                    }
                    CommentEditorEvent::Delete(id) => cx.emit(CommentChange::Remove(*id)),
                    CommentEditorEvent::Cancel => {}
                }
                this.close_file_comment(cx);
            },
        ));
        self.file_comment = Some(card);
        cx.notify();
    }

    fn close_file_comment(&mut self, cx: &mut Context<Self>) {
        if self.file_comment.take().is_some() {
            self._file_comment_subscription = None;
            cx.notify();
        }
    }

    fn open_menu(
        &mut self,
        path: String,
        is_dir: bool,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.menu = Some(EntryMenu {
            path,
            is_dir,
            position,
            confirm_delete: false,
            error: None,
        });
        cx.notify();
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    /// Copy `path`, absolute or relative to the project root.
    fn copy_path(&mut self, path: &str, absolute: bool, cx: &mut Context<Self>) {
        let text = match &self.root {
            Some(root) if absolute => absolute_path(root, path).display().to_string(),
            _ => path.to_owned(),
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.close_menu(cx);
    }

    fn delete_file(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let Some(service) = cx
            .try_global::<Gpui>()
            .and_then(|gpui| gpui.session_service())
        else {
            return;
        };
        self.delete_task = Some(cx.spawn(async move |this, cx| {
            let result = service
                .delete_project_file(session_id.clone(), PathBuf::from(&path))
                .await;
            this.update(cx, |this, cx| {
                if this.session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }
                match result {
                    Ok(()) => this.deleted(&path, cx),
                    Err(e) => {
                        if let Some(menu) = &mut this.menu {
                            menu.error = Some(format!("{e:#}"));
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// `path` was deleted: close it if it is open and list its directory
    /// again (without waiting for the watcher).
    fn deleted(&mut self, path: &str, cx: &mut Context<Self>) {
        self.menu = None;
        if self.viewer.read(cx).path() == Some(path) {
            self.close_file_comment(cx);
            self.viewer
                .update(cx, |viewer, cx| viewer.close_deleted(cx));
            self.persist(cx);
        }
        self.load_dir(parent(path).to_owned(), cx);
        self.all_files = None;
        self.refresh_matches(cx);
    }

    fn persist(&self, cx: &mut Context<Self>) {
        let Some(session_id) = &self.session_id else {
            return;
        };
        let viewer = self.viewer.read(cx);
        let state = ui_state::FilesViewState {
            open_file: viewer.path().map(str::to_owned),
            expanded: self.expanded.iter().cloned().collect(),
            show_ignored: self.show_ignored,
            renderers: viewer.renderer_picks().clone(),
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

    fn on_renderer_selected(
        &mut self,
        _: &Entity<SelectState<Vec<RendererItem>>>,
        event: &SelectEvent<Vec<RendererItem>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(selected) = event;
        match selected {
            // Only a change is a pick; the default stays unrecorded.
            Some(id) if *id != self.viewer.read(cx).renderer_id(cx) => {
                let id = id.clone();
                self.viewer
                    .update(cx, |viewer, cx| viewer.pick_renderer(id, cx));
                self.persist(cx);
            }
            Some(_) => {}
            // Nothing selected: show the viewer's renderer again.
            None => {
                self.renderer_shown = (Vec::new(), file_renderers::TEXT.to_owned());
                cx.notify();
            }
        }
    }

    /// Offer the open file's renderers in the dropdown.
    fn sync_renderer_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let viewer = self.viewer.read(cx);
        let options = viewer.renderer_options(cx);
        let selected = viewer.renderer_id(cx);
        let ids: Vec<String> = options.iter().map(|(id, _)| id.clone()).collect();
        if self.renderer_shown.0 == ids && self.renderer_shown.1 == selected {
            return;
        }
        let items = options
            .into_iter()
            .map(|(id, label)| RendererItem { id, label })
            .collect();
        self.renderer_select.update(cx, |select, cx| {
            select.set_items(items, window, cx);
            select.set_selected_value(&selected, window, cx);
        });
        self.renderer_shown = (ids, selected);
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
                let menu_open = self.menu.as_ref().is_some_and(|menu| menu.path == *path);
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
                let selector = format!("files-row-{path}");
                div()
                    .id(("files-tree-row", ix))
                    .debug_selector(move || selector.clone())
                    .h(px(22.))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_1()
                    .pl(indent(*depth))
                    .pr_2()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(theme.list_active))
                    .when(menu_open && !selected, |d| d.bg(theme.list_hover))
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
                    .on_mouse_down(MouseButton::Right, {
                        let path = path.clone();
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.open_menu(path.clone(), is_dir, event.position, cx)
                        })
                    })
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
            .on_mouse_down(MouseButton::Right, {
                let path = path.clone();
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.open_menu(path.clone(), false, event.position, cx)
                })
            })
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
        let choose_renderer = self.renderer_shown.0.len() > 1;
        let commented = self.root.as_ref().is_some_and(|root| {
            let file = absolute_path(root, &path);
            self.comments.iter().any(|c| c.on_file && c.file == file)
        });
        let bounds = self.header_bounds.clone();
        let probe = canvas(move |b, _, _| bounds.set(Some(b)), |_, _, _, _| {})
            .absolute()
            .size_full();
        let comment_path = path.clone();
        Some(
            div()
                .relative()
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
                .child(probe)
                .child(
                    Button::new("files-comment-file")
                        .icon(
                            Icon::default()
                                .path(SharedString::from("icons/message_circle_plus.svg"))
                                .with_size(Size::XSmall)
                                .when(commented, |icon| icon.text_color(theme.warning)),
                        )
                        .ghost()
                        .xsmall()
                        .tooltip(if commented {
                            "Edit the comment on this file"
                        } else {
                            "Comment on this file"
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.start_file_comment(comment_path.clone(), cx)
                        })),
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
                })
                .when(choose_renderer, |d| {
                    d.child(
                        div()
                            .flex_none()
                            .w(px(104.))
                            .child(Select::new(&self.renderer_select).with_size(Size::XSmall)),
                    )
                }),
        )
    }

    /// The open entry menu, floating where it was opened.
    fn render_menu(&self, cx: &mut Context<Self>) -> Option<gpui_kit::AnyElement> {
        let menu = self.menu.as_ref()?;
        let theme = cx.theme();
        let item = |id: &'static str, icon: &'static str, label: SharedString, danger: bool| {
            let color = if danger {
                theme.danger
            } else {
                theme.foreground
            };
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .text_color(color)
                .hover(|s| s.bg(theme.list_hover))
                .child(
                    Icon::default()
                        .path(SharedString::from(icon))
                        .with_size(Size::XSmall)
                        .text_color(if danger {
                            theme.danger
                        } else {
                            theme.muted_foreground
                        }),
                )
                .child(label)
        };
        let path = menu.path.clone();
        let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
        let body = if menu.confirm_delete {
            let delete_path = path.clone();
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .text_color(theme.foreground)
                        .child(format!("Delete {name}?")),
                )
                .children(menu.error.clone().map(|error| {
                    div()
                        .px_2()
                        .text_color(theme.danger)
                        .whitespace_normal()
                        .child(error)
                }))
                .child(
                    item(
                        "files-menu-confirm-delete",
                        "icons/trash.svg",
                        "Delete".into(),
                        true,
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.delete_file(delete_path.clone(), cx)
                        }),
                    ),
                )
                .child(
                    item(
                        "files-menu-cancel",
                        "icons/close.svg",
                        "Cancel".into(),
                        false,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.close_menu(cx))),
                )
        } else {
            let (absolute, relative, comment) = (path.clone(), path.clone(), path.clone());
            div()
                .flex()
                .flex_col()
                .child(
                    item(
                        "files-menu-copy-path",
                        "icons/copy.svg",
                        "Copy Path".into(),
                        false,
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.copy_path(&absolute, true, cx)),
                    ),
                )
                .child(
                    item(
                        "files-menu-copy-relative-path",
                        "icons/copy.svg",
                        "Copy Relative Path".into(),
                        false,
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.copy_path(&relative, false, cx)),
                    ),
                )
                .when(!menu.is_dir, |d| {
                    d.child(div().my_1().h(px(1.)).bg(theme.border))
                        .child(
                            item(
                                "files-menu-comment",
                                "icons/message_circle_plus.svg",
                                "Comment on File…".into(),
                                false,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.close_menu(cx);
                                    this.start_file_comment(comment.clone(), cx);
                                },
                            )),
                        )
                        .child(
                            item(
                                "files-menu-delete",
                                "icons/trash.svg",
                                "Delete File…".into(),
                                true,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(menu) = &mut this.menu {
                                    menu.confirm_delete = true;
                                }
                                cx.notify();
                            })),
                        )
                })
        };
        Some(floating(
            menu.position,
            div()
                .id("files-entry-menu")
                .occlude()
                .on_mouse_down_out(
                    cx.listener(|this, _: &MouseDownEvent, _, cx| this.close_menu(cx)),
                )
                .min_w(px(180.))
                .max_w(px(320.))
                .p_1()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .shadow_md()
                .text_xs()
                .child(body),
        ))
    }

    /// The card for a comment on the whole open file, under its path header.
    fn render_file_comment(
        &mut self,
        header: Option<Bounds<Pixels>>,
        window: &mut Window,
    ) -> Option<gpui_kit::AnyElement> {
        let position = self
            .file_comment
            .as_ref()
            .and(header)
            .map(|b| point(b.left() + px(8.), b.bottom() + px(4.)));
        self.file_comment_anchor.track(position, window);
        Some(floating(position?, self.file_comment.clone()?))
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
        self.sync_renderer_select(window, cx);
        if let Some(comment) = self.pending_file_comment.take() {
            self.open_file_comment(comment, window, cx);
        }
        // Read last frame's header position; the header sets it again when
        // it is drawn.
        let header = self.header_bounds.take();
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
            .children(self.render_menu(cx))
            .children(self.render_file_comment(header, window))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_assistant_core::session::DirEntry;
    use gpui_kit::{Modifiers, TestAppContext, VisualTestContext};

    /// A window with a `FilesView` of the project `/p` holding `src/a.rs`
    /// and the folder `docs`.
    fn view(cx: &mut TestAppContext) -> (Entity<FilesView>, &mut VisualTestContext) {
        let window = cx.update(|cx| {
            gpui_kit::component::init(cx);
            file_icons::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| FilesView::new(window, cx))
            })
            .unwrap()
        });
        let view = window.root(cx).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        view.update(cx, |view, cx| {
            let entry = |name: &str, kind| DirEntry {
                name: name.into(),
                kind,
                ignored: false,
            };
            view.root = Some(PathBuf::from("/p"));
            view.dirs.insert(
                String::new(),
                DirState::Loaded(DirListing {
                    entries: vec![entry("docs", EntryKind::Dir), entry("src", EntryKind::Dir)],
                    omitted: 0,
                }),
            );
            view.dirs.insert(
                "src".into(),
                DirState::Loaded(DirListing {
                    entries: vec![entry("a.rs", EntryKind::File)],
                    omitted: 0,
                }),
            );
            view.expanded.insert("src".into());
            view.rebuild_rows();
            cx.notify();
        });
        cx.run_until_parked();
        (view, cx)
    }

    fn right_click(selector: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx.debug_bounds(selector).expect(selector);
        cx.simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::none());
        cx.simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::none());
        cx.run_until_parked();
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx.debug_bounds(selector).expect(selector);
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();
    }

    fn clipboard(cx: &mut VisualTestContext) -> Option<String> {
        cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
    }

    #[gpui_kit::test]
    fn the_entry_menu_copies_paths(cx: &mut TestAppContext) {
        let (view, cx) = view(cx);
        right_click("files-row-src/a.rs", cx);
        click("files-menu-copy-path", cx);
        let absolute = PathBuf::from("/p").join("src").join("a.rs");
        assert_eq!(clipboard(cx), Some(absolute.display().to_string()));
        view.read_with(cx, |view, _| assert!(view.menu.is_none()));

        right_click("files-row-src/a.rs", cx);
        click("files-menu-copy-relative-path", cx);
        assert_eq!(clipboard(cx).as_deref(), Some("src/a.rs"));
    }

    #[gpui_kit::test]
    fn folders_offer_only_copying(cx: &mut TestAppContext) {
        let (_view, cx) = view(cx);
        right_click("files-row-docs", cx);
        assert!(cx.debug_bounds("files-menu-copy-path").is_some());
        assert!(cx.debug_bounds("files-menu-comment").is_none());
        assert!(cx.debug_bounds("files-menu-delete").is_none());
    }

    #[gpui_kit::test]
    fn deleting_asks_first(cx: &mut TestAppContext) {
        let (view, cx) = view(cx);
        right_click("files-row-src/a.rs", cx);
        click("files-menu-delete", cx);
        view.read_with(cx, |view, _| {
            assert!(view.menu.as_ref().is_some_and(|menu| menu.confirm_delete))
        });
        assert!(cx.debug_bounds("files-menu-confirm-delete").is_some());
        click("files-menu-cancel", cx);
        view.read_with(cx, |view, _| assert!(view.menu.is_none()));
    }

    #[gpui_kit::test]
    fn a_deleted_open_file_closes(cx: &mut TestAppContext) {
        let (view, cx) = view(cx);
        view.update(cx, |view, cx| {
            view.open_file("src/a.rs".into(), cx);
            view.deleted("src/a.rs", cx);
        });
        view.read_with(cx, |view, cx| {
            assert_eq!(view.viewer.read(cx).path(), None);
        });
    }

    #[gpui_kit::test]
    fn comments_on_a_whole_file_open_a_card_under_its_header(cx: &mut TestAppContext) {
        let (view, cx) = view(cx);
        right_click("files-row-src/a.rs", cx);
        click("files-menu-comment", cx);
        // The first frame draws the header, the next floats the card below.
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        view.read_with(cx, |view, cx| {
            assert_eq!(view.viewer.read(cx).path(), Some("src/a.rs"));
            let card = view.file_comment.as_ref().expect("the card opened");
            let comment = card.read(cx).comment();
            assert!(comment.on_file);
            assert_eq!(comment.file, PathBuf::from("/p/src/a.rs"));
            assert_eq!(comment.id, 0);
        });

        // An existing comment on the file is opened again, also when the
        // composer reveals it.
        let mut existing = LineComment::on_file("/p/src/a.rs");
        existing.id = 7;
        existing.text = "split this up".into();
        view.update(cx, |view, cx| {
            view.close_file_comment(cx);
            view.set_comments(vec![existing.clone()], cx);
            view.start_file_comment("src/a.rs".into(), cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, cx| {
            let card = view.file_comment.as_ref().expect("the card opened");
            assert_eq!(card.read(cx).comment().id, 7);
        });
        view.update(cx, |view, cx| {
            view.close_file_comment(cx);
            view.open_file("src/other.rs".into(), cx);
            view.reveal_comment(existing.clone(), cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, cx| {
            assert_eq!(view.viewer.read(cx).path(), Some("src/a.rs"));
            assert!(view.file_comment.is_some());
        });

        // Opening another file closes the card.
        view.update(cx, |view, cx| view.open_file("src/other.rs".into(), cx));
        view.read_with(cx, |view, _| assert!(view.file_comment.is_none()));
    }

    #[gpui_kit::test]
    fn renderers_from_the_settings_are_offered(cx: &mut TestAppContext) {
        use crate::shared::custom_renderers::{
            self, CommandRenderer, CustomRenderer, CustomRenderersConfig, OutputFormat,
            RendererKind,
        };
        let (view, cx) = view(cx);
        cx.update(|_, cx| {
            let config = CustomRenderersConfig {
                renderers: vec![CustomRenderer {
                    label: "Graphviz".into(),
                    files: vec!["*.dot".into()],
                    kind: RendererKind::Command(CommandRenderer {
                        command: "dot -Tsvg".into(),
                        output: OutputFormat::Svg,
                    }),
                }],
            };
            custom_renderers::set_current(&config, cx);
        });
        view.update(cx, |view, cx| view.open_file("docs/flow.dot".into(), cx));
        let labels = |view: &Entity<FilesView>, cx: &mut VisualTestContext| {
            view.read_with(cx, |view, cx| {
                let viewer = view.viewer.read(cx);
                let labels: Vec<String> = viewer
                    .renderer_options(cx)
                    .into_iter()
                    .map(|(_, label)| label.to_string())
                    .collect();
                (labels, viewer.renderer_id(cx))
            })
        };
        assert_eq!(
            labels(&view, cx),
            (
                vec!["Text".into(), "Graphviz".into()],
                "custom:Graphviz".into()
            )
        );

        // Removed in the settings: the file is text again.
        cx.update(|_, cx| custom_renderers::set_current(&CustomRenderersConfig::default(), cx));
        assert_eq!(labels(&view, cx), (vec!["Text".into()], "text".into()));
    }

    #[test]
    fn paths_join_and_split() {
        assert_eq!(join("", "a"), "a");
        assert_eq!(join("a/b", "c"), "a/b/c");
        assert_eq!(parent("a/b/c"), "a/b");
        assert_eq!(parent("a"), "");
    }
}
