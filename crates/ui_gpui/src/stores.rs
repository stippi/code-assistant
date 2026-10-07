//! Where the GPUI frontend keeps its own state across restarts.
//!
//! [`Gpui`](crate::Gpui) receives its stores instead of opening files itself,
//! so tests can inject in-memory stores (and simulate their failures).

use crate::shared::ui_state::{FileUiStatePersistence, UiStatePersistence};
use code_assistant_core::persistence::{DraftStore, FileDraftStore};
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
    /// The stores of the installed app: drafts in the user's config
    /// directory, UI states next to the session files.
    pub fn on_disk() -> Self {
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| std::env::current_dir().unwrap())
            .join("code-assistant");
        let sessions_dir = code_assistant_core::config_dir::data_dir().join("sessions");
        Self {
            drafts: Arc::new(FileDraftStore::new(config_dir)),
            ui_state: Arc::new(FileUiStatePersistence::new(sessions_dir)),
        }
    }
}
