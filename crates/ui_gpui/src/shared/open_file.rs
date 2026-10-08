//! Requests to show a project file in the right panel's Files view, from
//! anywhere in the UI (e.g. a file path in a tool card).
//!
//! Elements deep in the transcript cannot reach the right panel directly:
//! they emit an [`OpenFileRequest`] on the app-wide [`OpenFileBus`], which
//! the main screen subscribes to.

use gpui_kit::{
    App, Div, Entity, EventEmitter, Global, InteractiveElement, Stateful,
    StatefulInteractiveElement,
};

/// Show `path` (relative to the project root, or absolute inside it) and
/// select `line` (1-based) when given. `project` names the project the
/// path belongs to, when the source knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenFileRequest {
    pub path: String,
    pub line: Option<usize>,
    pub project: Option<String>,
}

pub struct OpenFileBus;

impl EventEmitter<OpenFileRequest> for OpenFileBus {}

#[derive(Clone)]
pub struct OpenFileGlobal(pub Entity<OpenFileBus>);

impl Global for OpenFileGlobal {}

/// Ask for `request` to be shown; a no-op before the main screen exists.
pub fn request(request: OpenFileRequest, cx: &mut App) {
    if let Some(bus) = cx.try_global::<OpenFileGlobal>().map(|g| g.0.clone()) {
        bus.update(cx, |_, cx| cx.emit(request));
    }
}

/// Make `element` open `request` on click, with a pointer cursor and an
/// underline on hover.
pub fn clickable(element: Stateful<Div>, request: OpenFileRequest) -> Stateful<Div> {
    use gpui_kit::Styled;
    element
        .cursor_pointer()
        .hover(|s| s.underline())
        .on_click(move |_, _, cx| self::request(request.clone(), cx))
}
