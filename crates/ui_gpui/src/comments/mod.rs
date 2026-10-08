//! Comments the user attaches to file lines or chat passages, as far as
//! they are shared between the views that make them: the floating selection
//! pill (copy, comment), the floating comment card ([`editor`]) and the
//! app-wide plumbing.
//!
//! The draft's comments live in the composer. The right panel's views report
//! changes through their own events; message blocks deep in the transcript
//! use the [`CommentBus`], which the main screen listens to. The current
//! comments are mirrored in the [`CurrentComments`] global so message blocks
//! can mark what was commented.

pub mod editor;

use code_assistant_core::line_comments::LineComment;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size};
use gpui_kit::{
    AnyElement, App, Entity, EventEmitter, Global, Pixels, Point, SharedString, Window, anchored,
    deferred, div, prelude::*, px,
};

/// A change to the draft's comments.
#[derive(Clone, Debug)]
pub enum CommentChange {
    /// Add (`id` 0) or replace a comment.
    Upsert(LineComment),
    Remove(u64),
}

/// Comment changes from anywhere in the UI.
pub struct CommentBus;

impl EventEmitter<CommentChange> for CommentBus {}

#[derive(Clone)]
pub struct CommentBusGlobal(pub Entity<CommentBus>);

impl Global for CommentBusGlobal {}

/// Report `change` on the [`CommentBus`]; a no-op before the main screen
/// exists.
pub fn report(change: CommentChange, cx: &mut App) {
    if let Some(bus) = cx.try_global::<CommentBusGlobal>().map(|g| g.0.clone()) {
        bus.update(cx, |_, cx| cx.emit(change));
    }
}

/// The comments of the shown session's draft.
#[derive(Default)]
pub struct CurrentComments(pub Vec<LineComment>);

impl Global for CurrentComments {}

/// The draft's comments on chat messages.
pub fn message_comments(cx: &App) -> Vec<LineComment> {
    cx.try_global::<CurrentComments>()
        .map(|c| c.0.iter().filter(|c| c.on_message).cloned().collect())
        .unwrap_or_default()
}

/// `child` floating at `position` (window coordinates) above everything,
/// kept inside the window.
pub fn floating(position: Point<Pixels>, child: impl IntoElement) -> AnyElement {
    deferred(
        anchored()
            .position(position)
            .snap_to_window_with_margin(px(8.))
            .child(child),
    )
    .with_priority(2)
    .into_any_element()
}

/// Keeps a floating element next to content that may move under it: the
/// position is computed from the last frame's layout, so when it moved,
/// one more frame is drawn to catch up.
#[derive(Default)]
pub struct AnchorTracker {
    last: Option<Point<Pixels>>,
}

impl AnchorTracker {
    /// Where the anchor was last seen.
    pub fn last(&self) -> Option<Point<Pixels>> {
        self.last
    }

    /// Note where the anchor is now; `None` when it is out of view.
    pub fn track(&mut self, position: Option<Point<Pixels>>, window: &mut Window) {
        if position != self.last {
            self.last = position;
            window.request_animation_frame();
        }
    }
}

/// The small pill shown at the end of a selection: copy and comment.
pub fn selection_pill(
    id: impl Into<SharedString>,
    on_copy: impl Fn(&mut Window, &mut App) + 'static,
    on_comment: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let id: SharedString = id.into();
    let theme = cx.theme();
    let button = |suffix: &str, icon: &'static str, tooltip: &'static str| {
        div()
            .id(SharedString::from(format!("{id}-{suffix}")))
            .debug_selector({
                let selector = format!("{id}-{suffix}");
                move || selector.clone()
            })
            .size(px(24.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .cursor_pointer()
            .hover(|s| s.bg(theme.muted))
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(tooltip).build(window, cx)
            })
            .child(
                Icon::default()
                    .path(SharedString::from(icon))
                    .with_size(Size::Small)
                    .text_color(theme.muted_foreground),
            )
    };
    div()
        .id(id.clone())
        .occlude()
        .flex()
        .items_center()
        .gap_0p5()
        .p_0p5()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .shadow_md()
        .child(
            button("copy", "icons/copy.svg", "Copy")
                .on_click(move |_, window, cx| on_copy(window, cx)),
        )
        .child(
            button("comment", "icons/message_circle_plus.svg", "Comment (⌘⇧M)")
                .on_click(move |_, window, cx| on_comment(window, cx)),
        )
}
