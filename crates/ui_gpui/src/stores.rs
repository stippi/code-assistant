//! Where the GPUI frontend keeps its own state across restarts.
//!
//! [`Gpui`](crate::Gpui) receives its stores instead of opening files itself,
//! so tests can inject in-memory stores (and simulate their failures).

use code_assistant_core::persistence::{DraftStore, FileDraftStore};
use std::sync::Arc;

/// The stores [`Gpui`](crate::Gpui) persists through.
#[derive(Clone)]
pub struct Stores {
    /// Unsent composer content per session.
    pub drafts: Arc<dyn DraftStore>,
}

impl Stores {
    /// The stores of the installed app, in the user's config directory.
    pub fn on_disk() -> Self {
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| std::env::current_dir().unwrap())
            .join("code-assistant");
        Self {
            drafts: Arc::new(FileDraftStore::new(config_dir)),
        }
    }
}
