//! Where the GPUI frontend keeps its own state across restarts.
//!
//! [`Gpui`](crate::Gpui) receives its stores instead of opening files itself,
//! so tests can inject in-memory stores (and simulate their failures).

use crate::shared::review_draft_store::{FileReviewDraftStore, ReviewDraftStore};
use crate::shared::reviewed_files_store::{FileReviewedFilesStore, ReviewedFilesStore};
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
    /// Unsent comments and reviews per pull request.
    pub review_drafts: Arc<dyn ReviewDraftStore>,
    /// The Review tab's "Viewed" ticks per repository.
    pub reviewed_files: Arc<dyn ReviewedFilesStore>,
}

impl Stores {
    /// The stores of the installed app: drafts and UI states in the
    /// session folders.
    pub fn on_disk() -> Self {
        let layout = FileSessionPersistence::new().layout().clone();
        Self {
            drafts: Arc::new(FileDraftStore::new(layout.clone())),
            review_drafts: Arc::new(FileReviewDraftStore::new(&layout)),
            reviewed_files: Arc::new(FileReviewedFilesStore::new(&layout)),
            ui_state: Arc::new(FileUiStatePersistence::new(layout)),
        }
    }
}
