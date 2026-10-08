//! One project file in a code editor: line numbers, syntax highlighting,
//! search, editing with auto-save, and line comments.
//!
//! Edits are saved once typing pauses for [`AUTO_SAVE_DELAY`], when another
//! file is opened, and at once on Cmd/Ctrl-S. While the file changed on disk
//! under unsaved edits, nothing is saved until the user picks a side.
//!
//! The text is read and written through the `SessionService`. The editor is
//! created in `render` once a load arrives (creating it needs the window).
//! A reload while there are no unsaved edits replaces the text in place and
//! keeps the scroll position; with unsaved edits the view only notes that the
//! file changed on disk and lets the user choose.
//!
//! Comments on this file show as tinted lines (editor decorations, which
//! follow edits). After a load or save each comment is found again by its
//! excerpt ([`line_comments::locate`]); one that moved is updated.

use super::comment_editor::{CommentChange, CommentEditor, CommentEditorEvent};
use crate::Gpui;
use crate::tool_cards::diff_syntax::language_for_path;
use code_assistant_core::line_comments::{self, LineComment};
use code_assistant_core::session::FileContent;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{
    Editor, EditorState, InputEvent, RopeExt, TextDecoration, TextDecorationCollection,
};
use gpui_kit::component::{ActiveTheme, Sizable, v_flex};
use gpui_kit::{
    Context, Entity, EventEmitter, FocusHandle, Focusable, HighlightStyle, KeyDownEvent, Render,
    Subscription, Task, Window, div, prelude::*,
};
use std::path::PathBuf;
use std::time::Duration;

/// Quiet period after the last edit before it is saved.
const AUTO_SAVE_DELAY: Duration = Duration::from_millis(1000);

/// What the viewer shows.
enum ViewerState {
    Empty,
    Loading,
    /// Text read from disk, waiting for `render` to put it into the editor.
    Loaded(String),
    Editing,
    Binary,
    TooLarge(u64),
    Failed(String),
}

pub struct FileViewer {
    session_id: Option<String>,
    /// The shown file, `/`-separated and relative to `root`.
    path: Option<String>,
    root: Option<PathBuf>,
    state: ViewerState,
    editor: Option<Entity<EditorState>>,
    /// Tinted comment lines.
    marks: Option<TextDecorationCollection>,
    /// The text as last read from or written to disk.
    saved_text: String,
    dirty: bool,
    /// The file changed on disk while there were unsaved edits.
    disk_changed: bool,
    save_error: Option<String>,
    /// Every comment of the draft; the ones on this file are marked.
    comments: Vec<LineComment>,
    comment_editor: Option<Entity<CommentEditor>>,
    /// A comment to select once the editor holds its file.
    pending_reveal: Option<LineComment>,
    /// A line (1-based) to select once the editor holds its file.
    pending_line: Option<usize>,
    load_task: Option<Task<()>>,
    save_task: Option<Task<()>>,
    auto_save_task: Option<Task<()>>,
    focus_handle: FocusHandle,
    _editor_subscriptions: Vec<Subscription>,
    _comment_editor_subscription: Option<Subscription>,
}

impl EventEmitter<CommentChange> for FileViewer {}

impl FileViewer {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            session_id: None,
            path: None,
            root: None,
            state: ViewerState::Empty,
            editor: None,
            marks: None,
            saved_text: String::new(),
            dirty: false,
            disk_changed: false,
            save_error: None,
            comments: Vec::new(),
            comment_editor: None,
            pending_reveal: None,
            pending_line: None,
            load_task: None,
            save_task: None,
            auto_save_task: None,
            focus_handle: cx.focus_handle(),
            _editor_subscriptions: Vec::new(),
            _comment_editor_subscription: None,
        }
    }

    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The shown file's absolute path, once its root is known.
    fn file(&self) -> Option<PathBuf> {
        Some(self.root.as_ref()?.join(self.path.as_ref()?))
    }

    /// Show `path` of `session_id`'s project; `None` shows nothing. Unsaved
    /// edits of the previous file are saved first.
    pub fn open(
        &mut self,
        session_id: Option<String>,
        path: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.session_id == session_id && self.path == path {
            return;
        }
        // Edits of the file being left are saved, not dropped.
        self.save_detached(cx);
        self.session_id = session_id;
        self.path = path;
        self.editor = None;
        self.marks = None;
        self._editor_subscriptions.clear();
        self.dirty = false;
        self.disk_changed = false;
        self.save_error = None;
        self.close_comment_editor(cx);
        self.state = if self.path.is_some() {
            ViewerState::Loading
        } else {
            ViewerState::Empty
        };
        self.load(cx);
        cx.notify();
    }

    /// Read the shown file again (it changed on disk).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.load(cx);
    }

    pub fn set_comments(&mut self, comments: Vec<LineComment>, cx: &mut Context<Self>) {
        if self.comments != comments {
            self.comments = comments;
            self.reanchor_and_mark(cx);
            cx.notify();
        }
    }

    /// Select `comment`'s lines and open it for editing, once the file is in
    /// the editor.
    pub fn reveal(&mut self, comment: LineComment, cx: &mut Context<Self>) {
        self.pending_reveal = Some(comment);
        cx.notify();
    }

    /// Select line `line` (1-based) once the file is in the editor.
    pub fn reveal_line(&mut self, line: usize, cx: &mut Context<Self>) {
        self.pending_line = Some(line);
        cx.notify();
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let (Some(session_id), Some(path)) = (self.session_id.clone(), self.path.clone()) else {
            self.load_task = None;
            return;
        };
        let Some(service) = cx
            .try_global::<Gpui>()
            .and_then(|gpui| gpui.session_service())
        else {
            return;
        };
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = service
                .read_project_file(session_id, PathBuf::from(&path))
                .await;
            this.update(cx, |this, cx| {
                if this.path.as_deref() != Some(path.as_str()) {
                    return;
                }
                match result {
                    Ok(file) => {
                        this.root = Some(file.root);
                        match file.content {
                            FileContent::Text(text) => this.loaded(text, cx),
                            FileContent::Binary => this.state = ViewerState::Binary,
                            FileContent::TooLarge { size } => {
                                this.state = ViewerState::TooLarge(size)
                            }
                        }
                    }
                    Err(e) => this.state = ViewerState::Failed(format!("{e:#}")),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn loaded(&mut self, text: String, cx: &mut Context<Self>) {
        if text == self.saved_text && matches!(self.state, ViewerState::Editing) {
            // Our own save coming back through the watcher.
            return;
        }
        if self.dirty && matches!(self.state, ViewerState::Editing) {
            let current = self.editor.as_ref().map(|e| e.read(cx).value().to_string());
            if current.as_deref() != Some(text.as_str()) {
                self.disk_changed = true;
            }
            return;
        }
        self.state = ViewerState::Loaded(text);
    }

    /// Put loaded text into the editor; called from `render`.
    fn apply_loaded(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.saved_text = text.clone();
        self.dirty = false;
        self.disk_changed = false;
        match &self.editor {
            Some(editor) => editor.update(cx, |editor, cx| {
                let scroll = editor.scroll_offset();
                let selection = editor.selected_range();
                editor.set_value(text, window, cx);
                editor.set_selected_range(selection, cx);
                editor.set_scroll_offset(scroll, cx);
            }),
            None => {
                let language = self
                    .path
                    .as_deref()
                    .and_then(language_for_path)
                    .unwrap_or("text");
                let editor = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language(language)
                        .line_number(true)
                        .searchable(true)
                        .default_value(text)
                });
                self._editor_subscriptions = vec![
                    cx.subscribe(&editor, |this, editor, event: &InputEvent, cx| {
                        if matches!(event, InputEvent::Change) {
                            let dirty = editor.read(cx).value().as_ref() != this.saved_text;
                            if dirty != this.dirty {
                                this.dirty = dirty;
                                cx.notify();
                            }
                            if dirty {
                                this.schedule_auto_save(cx);
                            }
                        }
                    }),
                    // Selection moves change what the header offers.
                    cx.observe(&editor, |_, _, cx| cx.notify()),
                ];
                self.marks = Some(editor.update(cx, |editor, cx| {
                    editor.create_decorations_collection(Vec::new(), cx)
                }));
                self.editor = Some(editor);
            }
        }
        self.state = ViewerState::Editing;
        self.reanchor_and_mark(cx);
    }

    /// Find this file's comments again in the editor's text, update the ones
    /// that moved, and tint their lines.
    fn reanchor_and_mark(&mut self, cx: &mut Context<Self>) {
        let (Some(editor), Some(marks), Some(file)) = (&self.editor, &self.marks, self.file())
        else {
            return;
        };
        let text = editor.read(cx).text().clone();
        let source = text.to_string();
        let lines: Vec<&str> = source.lines().collect();
        let color = cx.theme().warning.opacity(0.16);
        let mut decorations = Vec::new();
        let mut moved = Vec::new();
        for comment in self
            .comments
            .iter()
            .filter(|c| !c.in_diff && c.file == file)
        {
            let Some((start, end)) = line_comments::locate(&lines, comment) else {
                continue;
            };
            if (start, end) != (comment.start_line, comment.end_line) {
                let mut updated = comment.clone();
                updated.start_line = start;
                updated.end_line = end;
                moved.push(updated);
            }
            decorations.push(TextDecoration::new(
                text.line_start_offset(start - 1)..text.line_end_offset(end - 1),
                HighlightStyle {
                    background_color: Some(color),
                    ..Default::default()
                },
            ));
        }
        marks.set(decorations, cx);
        for comment in moved {
            cx.emit(CommentChange::Upsert(comment));
        }
    }

    /// The selected lines (1-based, inclusive) and their text; the cursor's
    /// line without a selection.
    fn selected_lines(&self, cx: &gpui_kit::App) -> Option<(usize, usize, String)> {
        let editor = self.editor.as_ref()?.read(cx);
        let text = editor.text();
        let range = editor.selected_range();
        let start = text.offset_to_point(range.start).row;
        let mut end = text.offset_to_point(range.end).row;
        // A selection ending at the start of a line does not include it.
        if end > start && text.line_start_offset(end) == range.end {
            end -= 1;
        }
        let excerpt = text
            .slice(text.line_start_offset(start)..text.line_end_offset(end))
            .to_string();
        Some((start + 1, end + 1, excerpt))
    }

    /// The comment on this file that covers the cursor's line, if any.
    fn comment_at_cursor(&self, cx: &gpui_kit::App) -> Option<LineComment> {
        let (start, end, _) = self.selected_lines(cx)?;
        let file = self.file()?;
        self.comments
            .iter()
            .find(|c| !c.in_diff && c.file == file && c.start_line <= end && start <= c.end_line)
            .cloned()
    }

    /// Whether the header offers to comment (a file is in the editor).
    pub fn can_comment(&self) -> bool {
        self.editor.is_some() && self.root.is_some()
    }

    /// Label of the header's comment button.
    pub fn comment_label(&self, cx: &gpui_kit::App) -> &'static str {
        if self.comment_at_cursor(cx).is_some() {
            "Edit comment"
        } else {
            "Comment"
        }
    }

    /// Open the comment editor for the selected lines (or the comment there).
    pub fn start_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let comment = match self.comment_at_cursor(cx) {
            Some(existing) => existing,
            None => {
                let (Some((start, end, excerpt)), Some(file)) =
                    (self.selected_lines(cx), self.file())
                else {
                    return;
                };
                LineComment {
                    id: 0,
                    file,
                    start_line: start,
                    end_line: end,
                    old_side: false,
                    in_diff: false,
                    excerpt,
                    text: String::new(),
                }
            }
        };
        self.open_comment_editor(comment, window, cx);
    }

    fn open_comment_editor(
        &mut self,
        comment: LineComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = cx.new(|cx| CommentEditor::new(comment, window, cx));
        self._comment_editor_subscription = Some(cx.subscribe_in(
            &editor,
            window,
            |this, _, event: &CommentEditorEvent, window, cx| {
                match event {
                    CommentEditorEvent::Save(comment) => {
                        cx.emit(CommentChange::Upsert(comment.clone()))
                    }
                    CommentEditorEvent::Delete(id) => cx.emit(CommentChange::Remove(*id)),
                    CommentEditorEvent::Cancel => {}
                }
                this.close_comment_editor(cx);
                if let Some(editor) = &this.editor {
                    editor.update(cx, |editor, cx| editor.focus(window, cx));
                }
            },
        ));
        self.comment_editor = Some(editor);
        cx.notify();
    }

    fn close_comment_editor(&mut self, cx: &mut Context<Self>) {
        self.comment_editor = None;
        self._comment_editor_subscription = None;
        cx.notify();
    }

    /// Select a revealed comment's lines and open it.
    fn apply_reveal(&mut self, comment: LineComment, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.editor else {
            return;
        };
        let lines = comment.start_line.max(1) - 1..comment.end_line.max(1) - 1;
        editor.update(cx, |editor, cx| {
            let text = editor.text();
            let last = text.lines_len().saturating_sub(1);
            let range = text.line_start_offset(lines.start.min(last))
                ..text.line_end_offset(lines.end.min(last));
            editor.set_selected_range(range, cx);
        });
        self.open_comment_editor(comment, window, cx);
    }

    /// Save once no edit came for [`AUTO_SAVE_DELAY`].
    fn schedule_auto_save(&mut self, cx: &mut Context<Self>) {
        self.auto_save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AUTO_SAVE_DELAY).await;
            this.update(cx, |this, cx| {
                if this.dirty && !this.disk_changed {
                    this.save(cx);
                }
            })
            .ok();
        }));
    }

    /// Write unsaved edits without waiting for the result (the view is about
    /// to show something else).
    fn save_detached(&mut self, cx: &mut Context<Self>) {
        self.auto_save_task = None;
        if !self.dirty || self.disk_changed {
            return;
        }
        let (Some(editor), Some(session_id), Some(path)) =
            (&self.editor, self.session_id.clone(), self.path.clone())
        else {
            return;
        };
        let Some(service) = cx
            .try_global::<Gpui>()
            .and_then(|gpui| gpui.session_service())
        else {
            return;
        };
        let text = editor.read(cx).value().to_string();
        cx.background_spawn(async move {
            if let Err(e) = service
                .write_project_file(session_id, PathBuf::from(&path), text)
                .await
            {
                tracing::warn!("Files view: could not save {path}: {e:#}");
            }
        })
        .detach();
        self.dirty = false;
    }

    pub fn save(&mut self, cx: &mut Context<Self>) {
        self.auto_save_task = None;
        let (Some(editor), Some(session_id), Some(path)) =
            (&self.editor, self.session_id.clone(), self.path.clone())
        else {
            return;
        };
        if !self.dirty && !self.disk_changed {
            return;
        }
        let Some(service) = cx
            .try_global::<Gpui>()
            .and_then(|gpui| gpui.session_service())
        else {
            return;
        };
        let text = editor.read(cx).value().to_string();
        self.save_task = Some(cx.spawn(async move |this, cx| {
            let result = service
                .write_project_file(session_id, PathBuf::from(&path), text.clone())
                .await;
            this.update(cx, |this, cx| {
                if this.path.as_deref() != Some(path.as_str()) {
                    return;
                }
                match result {
                    Ok(()) => {
                        this.saved_text = text;
                        this.dirty = this
                            .editor
                            .as_ref()
                            .is_some_and(|e| e.read(cx).value().as_ref() != this.saved_text);
                        this.disk_changed = false;
                        this.save_error = None;
                        this.reanchor_and_mark(cx);
                    }
                    Err(e) => this.save_error = Some(format!("Could not save: {e:#}")),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Drop unsaved edits and show the file as it is on disk.
    fn discard_edits(&mut self, cx: &mut Context<Self>) {
        self.dirty = false;
        self.disk_changed = false;
        self.load(cx);
        cx.notify();
    }

    fn notice(
        &self,
        text: impl Into<gpui_kit::SharedString>,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p_4()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(text.into())
            .into_any_element()
    }

    fn render_banner(&self, cx: &mut Context<Self>) -> Option<gpui_kit::AnyElement> {
        let theme = cx.theme();
        let (text, actions) = if let Some(error) = &self.save_error {
            (error.clone(), false)
        } else if self.disk_changed {
            ("The file changed on disk.".to_owned(), true)
        } else {
            return None;
        };
        Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .py_1()
                .bg(theme.warning.opacity(0.12))
                .text_xs()
                .text_color(theme.foreground)
                .child(div().flex_1().child(text))
                .when(actions, |d| {
                    d.child(
                        Button::new("file-reload-disk")
                            .label("Reload")
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| this.discard_edits(cx))),
                    )
                    .child(
                        Button::new("file-keep-mine")
                            .label("Overwrite")
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    )
                })
                .into_any_element(),
        )
    }
}

impl Focusable for FileViewer {
    fn focus_handle(&self, _cx: &gpui_kit::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for FileViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if matches!(self.state, ViewerState::Loaded(_)) {
            let ViewerState::Loaded(text) =
                std::mem::replace(&mut self.state, ViewerState::Editing)
            else {
                unreachable!()
            };
            self.apply_loaded(text, window, cx);
        }
        if self.editor.is_some()
            && let Some(comment) = self.pending_reveal.take()
        {
            self.apply_reveal(comment, window, cx);
        }
        if let Some(editor) = &self.editor
            && let Some(line) = self.pending_line.take()
        {
            editor.update(cx, |editor, cx| {
                let text = editor.text();
                let row = line.clamp(1, text.lines_len().max(1)) - 1;
                let range = text.line_start_offset(row)..text.line_end_offset(row);
                editor.set_selected_range(range, cx);
                editor.focus(window, cx);
            });
        }

        let body = match &self.state {
            ViewerState::Empty => self.notice("Select a file", cx),
            ViewerState::Loading | ViewerState::Loaded(_) => self.notice("Loading…", cx),
            ViewerState::Binary => self.notice("Binary file", cx),
            ViewerState::TooLarge(size) => {
                self.notice(format!("File too large to show ({} KB)", size / 1024), cx)
            }
            ViewerState::Failed(error) => self.notice(error.clone(), cx),
            ViewerState::Editing => match &self.editor {
                Some(editor) => Editor::new(editor)
                    .bordered(false)
                    .text_size(gpui_kit::rems(0.78125))
                    .font_family("Menlo")
                    .size_full()
                    .into_any_element(),
                None => self.notice("Loading…", cx),
            },
        };
        v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let ks = &event.keystroke;
                if ks.modifiers.secondary() && !ks.modifiers.shift && ks.key == "s" {
                    this.save(cx);
                    cx.stop_propagation();
                } else if ks.modifiers.secondary() && ks.modifiers.shift && ks.key == "m" {
                    this.start_comment(window, cx);
                    cx.stop_propagation();
                }
            }))
            .children(self.render_banner(cx))
            .child(div().flex_1().min_h_0().child(body))
            .children(self.comment_editor.clone())
    }
}
