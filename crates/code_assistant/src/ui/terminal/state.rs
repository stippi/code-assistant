use crate::persistence::ChatMetadata;
use crate::session::instance::SessionActivityState;
use crate::types::{PlanState, WorkingMemory};
use std::collections::HashMap;

pub struct AppState {
    pub working_memory: Option<WorkingMemory>,
    pub plan: Option<PlanState>,
    pub plan_expanded: bool,
    pub plan_dirty: bool,
    pub sessions: Vec<ChatMetadata>,
    pub current_session_id: Option<String>,
    pub activity_state: Option<SessionActivityState>,
    pub session_activity_states: HashMap<String, SessionActivityState>,
    pub pending_message: Option<String>,
    pub tool_statuses: HashMap<String, crate::ui::ToolStatus>,
    pub current_model: Option<String>,
    pub info_message: Option<String>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            working_memory: None,
            plan: None,
            plan_expanded: false,
            plan_dirty: true,
            sessions: Vec::new(),
            current_session_id: None,
            activity_state: None,
            session_activity_states: HashMap::new(),
            pending_message: None,
            tool_statuses: HashMap::new(),
            current_model: None,
            info_message: None,
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
        self.plan_expanded
    }
}
