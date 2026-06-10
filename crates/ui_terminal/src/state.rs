use code_assistant_core::persistence::ChatMetadata;
use code_assistant_core::session::instance::SessionActivityState;
use code_assistant_core::types::PlanState;
use sandbox::SandboxPolicy;
use std::collections::HashMap;

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
    /// Whether the slash-command autocomplete popup is currently visible.
    pub autocomplete_active: bool,
    /// The text the user has typed after the leading `/` on the current line.
    pub autocomplete_query: String,
    /// Index of the currently highlighted entry in the filtered command list.
    pub autocomplete_selected: usize,
}

impl AppState {
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
            autocomplete_active: false,
            autocomplete_query: String::new(),
            autocomplete_selected: 0,
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

    /// Open (or update) the autocomplete popup with a new query string.
    ///
    /// `query` is the text after the leading `/` on the current input line.
    /// Calling this resets the selection to the first item.
    pub fn open_autocomplete(&mut self, query: String) {
        self.autocomplete_active = true;
        self.autocomplete_query = query;
        self.autocomplete_selected = 0;
    }

    /// Close the autocomplete popup and reset all related state.
    pub fn close_autocomplete(&mut self) {
        self.autocomplete_active = false;
        self.autocomplete_query.clear();
        self.autocomplete_selected = 0;
    }

    /// Move the selection by `delta` rows (positive = down, negative = up),
    /// wrapping around within `[0, item_count)`.
    ///
    /// Does nothing when `item_count` is zero.
    pub fn move_autocomplete_selection(&mut self, delta: i32, item_count: usize) {
        if item_count == 0 {
            return;
        }
        let current = self.autocomplete_selected as i32;
        let next = (current + delta).rem_euclid(item_count as i32) as usize;
        self.autocomplete_selected = next;
    }
}
