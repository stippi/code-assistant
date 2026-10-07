//! Draft message persistence.
//!
//! Manages per-session draft text and attachments, using an in-memory cache
//! backed by the injected [`DraftStore`](code_assistant_core::persistence::DraftStore)
//! for persistence across restarts.

use super::super::Gpui;
use code_assistant_core::persistence::{DraftAttachment, NodeId, SessionDraft};
use tracing::warn;

impl Gpui {
    /// Save draft text, attachments and edit state for a session.
    ///
    /// `editing_branch_parent_id` is `Some(_)` when the user is editing an
    /// existing message (the value is the parent node where the new branch
    /// will be created). Persisting it lets the editing banner and truncated
    /// transcript be restored when the session is reconnected.
    ///
    /// Updates the in-memory cache immediately and schedules an async store write.
    pub fn save_draft_for_session(
        &self,
        session_id: &str,
        content: &str,
        attachments: &[DraftAttachment],
        editing_branch_parent_id: Option<NodeId>,
    ) {
        let draft = SessionDraft {
            session_id: session_id.to_string(),
            message: content.to_string(),
            attachments: attachments.to_vec(),
            editing_branch_parent_id,
        };
        let is_empty = draft.is_empty();

        // Update in-memory cache (text only)
        {
            let mut drafts = self.session_drafts.lock().unwrap();
            if is_empty {
                drafts.remove(session_id);
            } else {
                drafts.insert(session_id.to_string(), content.to_string());
            }
        }

        let draft_store = self.stores.drafts.clone();
        let session_drafts = self.session_drafts.clone();

        // On GPUI's executor: callers include the event bridge, which runs
        // outside any tokio runtime.
        self.dispatch(async move {
            // For empty drafts, always try to delete (idempotent)
            if is_empty {
                if let Err(e) = draft_store.delete(&draft.session_id) {
                    warn!(
                        "Failed to delete draft for session {}: {}",
                        draft.session_id, e
                    );
                }
                return;
            }

            // For non-empty content, check cache right before the write to
            // avoid races with newer edits. Always save when there is an edit
            // state or attachments, even if the text was cleared.
            let still_current =
                session_drafts.lock().unwrap().get(&draft.session_id) == Some(&draft.message);

            if (still_current
                || !draft.attachments.is_empty()
                || draft.editing_branch_parent_id.is_some())
                && let Err(e) = draft_store.save(&draft)
            {
                warn!(
                    "Failed to save draft for session {}: {}",
                    draft.session_id, e
                );
            }
        });
    }

    /// Load draft text, attachments and edit state for a session.
    ///
    /// Loads the full draft from the store; when that has none (or fails),
    /// falls back to text that was cached but not yet written.
    pub fn load_draft_for_session(&self, session_id: &str) -> Option<SessionDraft> {
        let cached_text = self.session_drafts.lock().unwrap().get(session_id).cloned();
        let cached_draft = || {
            cached_text.map(|message| SessionDraft {
                session_id: session_id.to_string(),
                message,
                attachments: Vec::new(),
                editing_branch_parent_id: None,
            })
        };

        match self.stores.drafts.load(session_id) {
            Ok(Some(draft)) => {
                self.session_drafts
                    .lock()
                    .unwrap()
                    .insert(session_id.to_string(), draft.message.clone());
                Some(draft)
            }
            Ok(None) => cached_draft(),
            Err(e) => {
                warn!("Failed to load draft for session {}: {}", session_id, e);
                cached_draft()
            }
        }
    }

    /// Clear draft for a session from both cache and store.
    pub fn clear_draft_for_session(&self, session_id: &str) {
        // Remove from in-memory cache FIRST
        self.session_drafts.lock().unwrap().remove(session_id);

        // Cleared synchronously so it happens before any racing save operations
        if let Err(e) = self.stores.drafts.delete(session_id) {
            warn!("Failed to clear draft for session {}: {}", session_id, e);
        }
    }
}
