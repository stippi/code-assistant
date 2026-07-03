use crate::slash_popup::PopupStack;
use code_assistant_core::persistence::{ChatMetadata, NodeId};
use code_assistant_core::session::instance::SessionActivityState;
use code_assistant_core::session::service::SkillCatalogEntry;
use code_assistant_core::types::PlanState;
use sandbox::SandboxPolicy;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayState {
    None,
    Plan,
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
    pub info_message: Option<String>,
    pub current_sandbox_policy: Option<SandboxPolicy>,
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

    pub fn set_info_message(&mut self, message: Option<String>) {
        self.info_message = message;
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
}
