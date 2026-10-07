//! Where the GPUI frontend keeps its own state across restarts.
//!
//! [`Gpui`](crate::Gpui) receives its stores instead of opening files itself,
//! so tests can inject in-memory stores (and simulate their failures).

use crate::shared::ui_state::{FileUiStatePersistence, UiStatePersistence};
use code_assistant_core::persistence::{DraftStore, FileDraftStore, FileSessionPersistence};
use std::sync::Arc;

/// The stores [`Gpui`](crate::Gpui) persists through.
#[derive(Clone)]
pub struct Stores {
    /// Unsent composer content per session.
    pub drafts: Arc<dyn DraftStore>,
    /// Per-session view state (collapsed blocks, scroll position, panels).
    pub ui_state: Arc<dyn UiStatePersistence>,
}

impl Stores {
    /// The stores of the installed app: drafts and UI states in the
    /// session folders.
    pub fn on_disk() -> Self {
        let layout = FileSessionPersistence::new().layout().clone();
        Self {
            drafts: Arc::new(FileDraftStore::new(layout.clone())),
            ui_state: Arc::new(FileUiStatePersistence::new(layout)),
        }
    }
}
