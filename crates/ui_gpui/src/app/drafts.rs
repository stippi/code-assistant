//! Draft message persistence.
//!
//! Manages per-session draft text and attachments, using an in-memory cache
//! backed by on-disk storage for persistence across restarts.

use super::super::Gpui;
use code_assistant_core::persistence::NodeId;
use tracing::warn;

impl Gpui {
    /// Save draft text, attachments and edit state for a session.
    ///
    /// `editing_branch_parent_id` is `Some(_)` when the user is editing an
    /// existing message (the value is the parent node where the new branch
    /// will be created). Persisting it lets the editing banner and truncated
    /// transcript be restored when the session is reconnected.
    ///
    /// Updates the in-memory cache immediately and schedules an async disk write.
    pub fn save_draft_for_session(
        &self,
        session_id: &str,
        content: &str,
        attachments: &[code_assistant_core::persistence::DraftAttachment],
        editing_branch_parent_id: Option<NodeId>,
    ) {
        // A draft is only "empty" (and therefore removable) when it has no
        // text, no attachments AND no edit state.
        let is_empty =
            content.is_empty() && attachments.is_empty() && editing_branch_parent_id.is_none();

        // Update in-memory cache (text only)
        {
            let mut drafts = self.session_drafts.lock().unwrap();
            if is_empty {
                drafts.remove(session_id);
            } else {
                drafts.insert(session_id.to_string(), content.to_string());
            }
        }

        // Save to disk (non-blocking) with full draft structure
        let draft_storage = self.draft_storage.clone();
        let session_id_owned = session_id.to_string();
        let content_owned = content.to_string();
        let attachments_owned = attachments.to_vec();
        let session_drafts = self.session_drafts.clone();

        tokio::spawn(async move {
            // For empty drafts, always try to delete (idempotent)
            if is_empty {
                if let Err(e) = draft_storage.save_draft(
                    &session_id_owned,
                    &content_owned,
                    &attachments_owned,
                    editing_branch_parent_id,
                ) {
                    warn!(
                        "Failed to delete draft for session {}: {}",
                        session_id_owned, e
                    );
                }
                return;
            }

            // For non-empty content, check cache right before disk write to
            // avoid races with newer edits. Always save when there is an edit
            // state or attachments, even if the text was cleared.
            let should_save = {
                let drafts = session_drafts.lock().unwrap();
                let exists_in_cache = drafts.contains_key(&session_id_owned);
                let current_content = drafts.get(&session_id_owned);

                // Only save if draft still exists in cache AND content matches exactly
                exists_in_cache && current_content == Some(&content_owned)
            };

            if should_save || !attachments_owned.is_empty() || editing_branch_parent_id.is_some() {
                if let Err(e) = draft_storage.save_draft(
                    &session_id_owned,
                    &content_owned,
                    &attachments_owned,
                    editing_branch_parent_id,
                ) {
                    warn!(
                        "Failed to save draft for session {}: {}",
                        session_id_owned, e
                    );
                }
            }
        });
    }

    /// Load draft text, attachments and edit state for a session.
    ///
    /// Checks the in-memory cache for text first, then loads the full draft
    /// structure (including the edit anchor) from disk.
    pub fn load_draft_for_session(
        &self,
        session_id: &str,
    ) -> Option<(
        String,
        Vec<code_assistant_core::persistence::DraftAttachment>,
        Option<NodeId>,
    )> {
        // First check in-memory cache for text
        let cached_text = {
            let drafts = self.session_drafts.lock().unwrap();
            drafts.get(session_id).cloned()
        };

        // Load the full draft structure from disk (text + attachments + edit anchor)
        match self.draft_storage.load_draft_struct(session_id) {
            Ok(Some(draft)) => {
                let draft_text = draft.get_message();
                // Cache the loaded draft text
                {
                    let mut drafts = self.session_drafts.lock().unwrap();
                    drafts.insert(session_id.to_string(), draft_text.clone());
                }
                Some((
                    draft_text,
                    draft.attachments,
                    draft.editing_branch_parent_id,
                ))
            }
            Ok(None) => {
                // Check if we have cached text without attachments/edit state
                cached_text.map(|text| (text, Vec::new(), None))
            }
            Err(e) => {
                warn!("Failed to load draft for session {}: {}", session_id, e);
                // Fallback to cached text if available
                cached_text.map(|text| (text, Vec::new(), None))
            }
        }
    }

    /// Clear draft for a session from both cache and disk.
    pub fn clear_draft_for_session(&self, session_id: &str) {
        // Remove from in-memory cache FIRST
        {
            let mut drafts = self.session_drafts.lock().unwrap();
            drafts.remove(session_id);
        }

        // Clear from disk synchronously to ensure it happens before any racing save operations
        if let Err(e) = self.draft_storage.clear_draft(session_id) {
            warn!("Failed to clear draft for session {}: {}", session_id, e);
        }
    }
}
