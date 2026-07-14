use crate::slash_popup::PopupStack;
use code_assistant_core::persistence::{ChatMetadata, NodeId};
use code_assistant_core::session::instance::SessionActivityState;
use code_assistant_core::session::permissions::ToolPermissionRequestData;
use code_assistant_core::session::service::SkillCatalogEntry;
use code_assistant_core::types::PlanState;
use sandbox::SandboxPolicy;
use std::collections::{HashMap, HashSet};
use tools_core::permissions::PermissionTier;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayState {
    None,
    Plan,
}

/// A transient status/confirmation shown below the composer (or, when it does
/// not fit on one row, above the input). Auto-dismisses at `expires_at`; a
/// `None` deadline means it stays until explicitly cleared (e.g. a permission
/// prompt that must remain until it is answered).
#[derive(Debug, Clone)]
pub struct InfoMessage {
    pub text: String,
    pub expires_at: Option<std::time::Instant>,
}

pub struct AppState {
    pub plan: Option<PlanState>,
    pub plan_expanded: bool,
    pub overlay_state: OverlayState,
    pub plan_dirty: bool,
    pub sessions: Vec<ChatMetadata>,
    pub current_session_id: Option<String>,
    pub activity_state: Option<SessionActivityState>,
    pub session_activity_states: HashMap<String, SessionActivityState>,
    pub pending_message: Option<String>,
    pub tool_statuses: HashMap<String, code_assistant_core::ui::ToolStatus>,
    pub current_model: Option<String>,
    pub info_message: Option<InfoMessage>,
    pub current_sandbox_policy: Option<SandboxPolicy>,
    pub current_permission_tier: Option<PermissionTier>,
    /// Tool permission requests awaiting a decision (FIFO; `/allow`,
    /// `/always` and `/deny` answer the oldest).
    pub pending_permission_requests: Vec<ToolPermissionRequestData>,
    /// Skills available to the current session, cached for the `/skill` picker.
    pub skills: Vec<SkillCatalogEntry>,
    /// Slash-command popup stack. Empty stack ↔ no popup visible.
    pub popup_stack: PopupStack,
    /// Node ids of messages the transcript already shows (or knows about),
    /// for deduplicating externally appended messages against locally
    /// streamed content.
    seen_node_ids: HashSet<NodeId>,
}

impl AppState {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            plan: None,
            plan_expanded: false,
            overlay_state: OverlayState::None,
            plan_dirty: true,
            sessions: Vec::new(),
            current_session_id: None,
            activity_state: None,
            session_activity_states: HashMap::new(),
            pending_message: None,
            tool_statuses: HashMap::new(),
            current_model: None,
            info_message: None,
            current_sandbox_policy: None,
            current_permission_tier: None,
            pending_permission_requests: Vec::new(),
            skills: Vec::new(),
            popup_stack: PopupStack::new(),
            seen_node_ids: HashSet::new(),
        }
    }

    pub fn update_sessions(&mut self, sessions: Vec<ChatMetadata>) {
        self.sessions = sessions;
    }

    pub fn update_activity_state(&mut self, activity_state: Option<SessionActivityState>) {
        self.activity_state = activity_state;
    }

    pub fn update_pending_message(&mut self, message: Option<String>) {
        self.pending_message = message;
    }

    pub fn update_session_activity_state(
        &mut self,
        session_id: String,
        activity_state: SessionActivityState,
    ) {
        self.session_activity_states
            .insert(session_id, activity_state);
    }

    pub fn update_current_model(&mut self, model: Option<String>) {
        self.current_model = model;
    }

    /// Record a message node the transcript already shows (or knows about).
    /// Returns `true` if the node was new — i.e. its message should be
    /// rendered. Keeps externally appended messages (file watcher) idempotent
    /// against locally streamed content, which carries the same
    /// pre-allocated node id.
    pub fn mark_node_seen(&mut self, node_id: NodeId) -> bool {
        self.seen_node_ids.insert(node_id)
    }

    /// Reset the seen-node set to the given ids (used when the transcript
    /// baseline is replaced, e.g. `SetMessages` on connect).
    pub fn reset_seen_nodes(&mut self, node_ids: impl IntoIterator<Item = NodeId>) {
        self.seen_node_ids = node_ids.into_iter().collect();
    }

    pub fn update_sandbox_policy(&mut self, policy: Option<SandboxPolicy>) {
        self.current_sandbox_policy = policy;
    }

    pub fn update_permission_tier(&mut self, tier: Option<PermissionTier>) {
        self.current_permission_tier = tier;
    }

    pub fn push_permission_request(&mut self, request: ToolPermissionRequestData) {
        if !self
            .pending_permission_requests
            .iter()
            .any(|r| r.request_id == request.request_id)
        {
            self.pending_permission_requests.push(request);
        }
    }

    pub fn remove_permission_request(&mut self, request_id: &str) {
        self.pending_permission_requests
            .retain(|r| r.request_id != request_id);
    }

    /// Show the modal prompt for the oldest pending permission request, one
    /// at a time, and keep the info banner (with the slash-command fallback
    /// for a dismissed prompt) in sync. No-op while a prompt is already open.
    pub fn open_next_permission_prompt(&mut self) {
        let Some(next) = self.pending_permission_requests.first().cloned() else {
            self.set_info_message(None);
            return;
        };
        self.set_sticky_info_message(format!(
            "Permission required: {} — /allow, /always or /deny",
            next.summary
        ));
        if !self.popup_stack.has_permission_popup() {
            self.popup_stack.push(Box::new(
                crate::slash_popup::PermissionPromptPopup::for_request(&next),
            ));
        }
    }

    /// Auto-dismiss duration for an info message: a few seconds, extended for
    /// longer (multi-line) content so lists stay readable.
    fn info_ttl(text: &str) -> std::time::Duration {
        let extra_lines = text.lines().count().saturating_sub(1) as u64;
        let ms = (4000 + 600 * extra_lines).min(20_000);
        std::time::Duration::from_millis(ms)
    }

    /// Set (or clear) the info message. A `Some` message auto-dismisses after
    /// [`Self::info_ttl`]; pass `None` to clear immediately.
    pub fn set_info_message(&mut self, message: Option<String>) {
        self.info_message = message.map(|text| {
            let expires_at = Some(std::time::Instant::now() + Self::info_ttl(&text));
            InfoMessage { text, expires_at }
        });
    }

    /// Set a sticky info message that stays until explicitly cleared (used for
    /// prompts, e.g. a pending permission request).
    pub fn set_sticky_info_message(&mut self, text: String) {
        self.info_message = Some(InfoMessage {
            text,
            expires_at: None,
        });
    }

    pub fn has_info(&self) -> bool {
        self.info_message.is_some()
    }

    pub fn info_text(&self) -> Option<&str> {
        self.info_message.as_ref().map(|m| m.text.as_str())
    }

    /// Clear the info message if its auto-dismiss deadline has passed.
    pub fn expire_info_message(&mut self, now: std::time::Instant) {
        if self
            .info_message
            .as_ref()
            .and_then(|m| m.expires_at)
            .is_some_and(|deadline| now >= deadline)
        {
            self.info_message = None;
        }
    }

    /// Time until the info message auto-dismisses, if it has a deadline.
    pub fn info_remaining(&self, now: std::time::Instant) -> Option<std::time::Duration> {
        self.info_message
            .as_ref()
            .and_then(|m| m.expires_at)
            .map(|deadline| deadline.saturating_duration_since(now))
    }

    pub fn set_plan(&mut self, plan: Option<PlanState>) {
        if let Some(ref plan_state) = plan {
            tracing::debug!(
                "AppState::set_plan with {} entries (expanded: {})",
                plan_state.entries.len(),
                self.plan_expanded
            );
        } else {
            tracing::debug!("AppState::set_plan clearing plan state");
        }
        self.plan = plan;
        self.plan_dirty = true;
    }

    pub fn toggle_plan_expanded(&mut self) -> bool {
        self.plan_expanded = !self.plan_expanded;
        self.overlay_state = if self.plan_expanded {
            OverlayState::Plan
        } else {
            OverlayState::None
        };
        self.plan_expanded
    }

    pub fn is_overlay_active(&self) -> bool {
        !matches!(self.overlay_state, OverlayState::None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seen_nodes_deduplicate_and_reset() {
        let mut state = AppState::new();

        // First sighting renders, repeats don't.
        assert!(state.mark_node_seen(42));
        assert!(!state.mark_node_seen(42));
        assert!(state.mark_node_seen(43));

        // Replacing the transcript baseline reseeds the set.
        state.reset_seen_nodes([1, 2]);
        assert!(!state.mark_node_seen(1));
        assert!(state.mark_node_seen(42));
    }

    #[test]
    fn info_message_auto_dismisses_after_deadline() {
        use std::time::{Duration, Instant};

        let mut state = AppState::new();
        state.set_info_message(Some("Switched to session: Foo".to_string()));
        assert!(state.has_info());
        let now = Instant::now();
        assert!(state.info_remaining(now).is_some());

        // Not yet expired.
        state.expire_info_message(now);
        assert!(state.has_info());

        // Past the deadline it clears.
        state.expire_info_message(now + Duration::from_secs(60));
        assert!(!state.has_info());
    }

    #[test]
    fn sticky_info_message_never_expires() {
        use std::time::{Duration, Instant};

        let mut state = AppState::new();
        state.set_sticky_info_message("Permission required: …".to_string());
        assert!(state.has_info());
        assert!(state.info_remaining(Instant::now()).is_none());

        // Even far in the future it stays until explicitly cleared.
        state.expire_info_message(Instant::now() + Duration::from_secs(3600));
        assert!(state.has_info());
        state.set_info_message(None);
        assert!(!state.has_info());
    }

    #[test]
    fn multi_line_info_gets_longer_ttl() {
        let single = AppState::info_ttl("one line");
        let many = AppState::info_ttl("a\nb\nc\nd\ne\nf");
        assert!(many > single, "multi-line info should linger longer");
    }
}
