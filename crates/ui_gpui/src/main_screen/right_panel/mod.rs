//! The right sidebar's view switcher.
//!
//! The sidebar hosts the [`review_view::ReviewView`], the project's files
//! ([`files_view::FilesView`]) and, with the `browser-panel` feature, the
//! agent's browser ([`browser::BrowserPanel`]), switched in a header above
//! them. The shown view is remembered per session.

#[cfg(feature = "browser-panel")]
mod browser;
pub mod comment_editor;
mod file_filter;
mod file_viewer;
pub mod files_view;
mod line_selection;
mod review_rows;
pub mod review_view;

use code_assistant_core::line_comments::LineComment;
pub use comment_editor::CommentChange;
use files_view::FilesView;
use gpui_kit::{Context, Entity, FocusHandle, Focusable, Render, Window, div, prelude::*};
use review_view::ReviewView;

/// Which view the right panel is currently showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RightPanelView {
    Review,
    Files,
    #[cfg(feature = "browser-panel")]
    Browser,
}

impl RightPanelView {
    /// Stable string used for persistence.
    pub fn as_str(self) -> &'static str {
        match self {
            RightPanelView::Review => "review",
            RightPanelView::Files => "files",
            #[cfg(feature = "browser-panel")]
            RightPanelView::Browser => "browser",
        }
    }

    /// Parse a persisted string back into a view (defaults to `Review`).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "files" => RightPanelView::Files,
            #[cfg(feature = "browser-panel")]
            "browser" => RightPanelView::Browser,
            _ => RightPanelView::Review,
        }
    }
}

pub struct RightPanel {
    active_view: RightPanelView,
    review_view: Entity<ReviewView>,
    files_view: Entity<FilesView>,
    #[cfg(feature = "browser-panel")]
    browser: Entity<browser::BrowserPanel>,
    session_id: Option<String>,
    #[cfg(feature = "browser-panel")]
    _browser_events: gpui_kit::Subscription,
    _comment_subscriptions: Vec<gpui_kit::Subscription>,
    focus_handle: FocusHandle,
}

/// The views report comment edits; the main screen applies them to the
/// composer, which owns the draft's comments.
impl gpui_kit::EventEmitter<CommentChange> for RightPanel {}

impl RightPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let review_view = cx.new(|cx| ReviewView::new(window, cx));
        let files_view = cx.new(|cx| FilesView::new(window, cx));
        let comment_subscriptions = vec![
            cx.subscribe(&review_view, |_, _, change: &CommentChange, cx| {
                cx.emit(change.clone())
            }),
            cx.subscribe(&files_view, |_, _, change: &CommentChange, cx| {
                cx.emit(change.clone())
            }),
        ];
        #[cfg(feature = "browser-panel")]
        let browser = cx.new(browser::BrowserPanel::new);
        Self {
            active_view: RightPanelView::Review,
            review_view,
            files_view,
            #[cfg(feature = "browser-panel")]
            _browser_events: cx.subscribe(&browser, |this, _, event, cx| match event {
                // The agent started browsing: show it.
                browser::BrowserPanelEvent::BrowserOpened => {
                    this.set_active_view(RightPanelView::Browser, cx)
                }
            }),
            #[cfg(feature = "browser-panel")]
            browser,
            session_id: None,
            _comment_subscriptions: comment_subscriptions,
            focus_handle: cx.focus_handle(),
        }
    }

    /// The draft's comments, for the views' markers.
    pub fn set_comments(&mut self, comments: Vec<LineComment>, cx: &mut Context<Self>) {
        self.files_view
            .update(cx, |v, cx| v.set_comments(comments.clone(), cx));
        self.review_view
            .update(cx, |v, cx| v.set_comments(comments, cx));
    }

    /// The file the Files view shows, if any.
    #[cfg(test)]
    pub fn files_open_path(&self, cx: &gpui_kit::App) -> Option<String> {
        self.files_view
            .read(cx)
            .viewer()
            .read(cx)
            .path()
            .map(str::to_owned)
    }

    /// Show `path` (project-relative or absolute) in the Files view, with
    /// `line` (1-based) selected when given.
    pub fn open_file(&mut self, path: String, line: Option<usize>, cx: &mut Context<Self>) {
        self.set_active_view(RightPanelView::Files, cx);
        self.files_view
            .update(cx, |v, cx| v.open_path(path, line, cx));
    }

    /// Show where `comment` was made and open it for editing.
    pub fn reveal_comment(&mut self, comment: LineComment, cx: &mut Context<Self>) {
        if comment.in_diff {
            self.set_active_view(RightPanelView::Review, cx);
            self.review_view
                .update(cx, |v, cx| v.reveal_comment(comment, cx));
        } else {
            self.set_active_view(RightPanelView::Files, cx);
            self.files_view
                .update(cx, |v, cx| v.reveal_comment(comment, cx));
        }
    }

    #[allow(dead_code)]
    pub fn active_view(&self) -> RightPanelView {
        self.active_view
    }

    pub fn set_active_view(&mut self, view: RightPanelView, cx: &mut Context<Self>) {
        if self.active_view == view {
            return;
        }
        self.active_view = view;
        if let Some(session_id) = &self.session_id {
            crate::shared::ui_state::update(cx, |store| {
                store.set_right_panel_view(session_id, view.as_str())
            });
        }
        self.sync_files_session(cx);
        cx.notify();
    }

    /// Point the views at a session.
    pub fn set_session(&mut self, session_id: Option<String>, cx: &mut Context<Self>) {
        self.active_view = session_id
            .as_deref()
            .and_then(|id| {
                crate::shared::ui_state::read(cx, |store| store.get_right_panel_view(id)).flatten()
            })
            .map_or(RightPanelView::Review, |view| {
                RightPanelView::from_str(&view)
            });
        self.session_id = session_id.clone();
        #[cfg(feature = "browser-panel")]
        self.browser.update(cx, |browser, cx| {
            browser.set_session(session_id.clone(), cx)
        });
        self.sync_files_session(cx);
        self.review_view
            .update(cx, |v, cx| v.set_session(session_id, cx));
        cx.notify();
    }

    /// The Files view follows the session only while it is shown, so a
    /// hidden one neither lists nor watches.
    fn sync_files_session(&mut self, cx: &mut Context<Self>) {
        let session_id = (self.active_view == RightPanelView::Files)
            .then(|| self.session_id.clone())
            .flatten();
        self.files_view
            .update(cx, |files, cx| files.set_session(session_id, cx));
    }

    /// Re-request data for the active view.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.review_view.update(cx, |v, cx| v.reload(cx));
        if self.active_view == RightPanelView::Files {
            self.files_view.update(cx, |v, cx| v.reload(cx));
        }
    }

    fn render_header(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::component::ActiveTheme;
        let panel = cx.entity().downgrade();
        let views: &[(RightPanelView, &str)] = &[
            (RightPanelView::Review, "Review"),
            (RightPanelView::Files, "Files"),
            #[cfg(feature = "browser-panel")]
            (RightPanelView::Browser, "Browser"),
        ];
        div()
            .flex_none()
            .flex()
            .items_center()
            .px_2()
            .py(gpui_kit::px(8.))
            .bg(cx.theme().title_bar)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(crate::shared::segmented_switch::segmented_switch(
                "right-panel-view",
                views,
                self.active_view,
                move |view, _, cx| {
                    panel
                        .update(cx, |panel, cx| panel.set_active_view(view, cx))
                        .ok();
                },
                window,
                cx,
            ))
    }
}

impl Focusable for RightPanel {
    fn focus_handle(&self, _cx: &gpui_kit::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RightPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.active_view {
            RightPanelView::Review => self.review_view.clone().into_any_element(),
            RightPanelView::Files => self.files_view.clone().into_any_element(),
            #[cfg(feature = "browser-panel")]
            RightPanelView::Browser => self.browser.clone().into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(self.render_header(window, cx))
            .child(div().flex_1().min_h_0().child(body))
    }
}
