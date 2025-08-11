use crate::persistence::ChatMetadata;
use crate::session::instance::SessionActivityState;
use crate::types::WorkingMemory;
use crate::ui::ui_events::MessageData;
use std::collections::HashMap;

pub struct AppState {
    pub messages: Vec<MessageData>,
    pub working_memory: Option<WorkingMemory>,
    pub sessions: Vec<ChatMetadata>,
    pub current_session_id: Option<String>,
    pub activity_state: Option<SessionActivityState>,
    pub session_activity_states: HashMap<String, SessionActivityState>,
    pub pending_message: Option<String>,
    pub tool_statuses: HashMap<String, crate::ui::ToolStatus>,
    #[allow(dead_code)]
    pub rate_limited: bool,
    #[allow(dead_code)]
    pub rate_limit_seconds: u64,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            working_memory: None,
            sessions: Vec::new(),
            current_session_id: None,
            activity_state: None,
            session_activity_states: HashMap::new(),
            pending_message: None,
            tool_statuses: HashMap::new(),
            rate_limited: false,
            rate_limit_seconds: 0,
        }
    }

    pub fn clear_messages(&mut self) {
        self.messages.clear();
    }

    pub fn add_message(&mut self, message: MessageData) {
        self.messages.push(message);
    }

    pub fn update_working_memory(&mut self, memory: WorkingMemory) {
        self.working_memory = Some(memory);
    }

    pub fn update_sessions(&mut self, sessions: Vec<ChatMetadata>) {
        self.sessions = sessions;
    }

    pub fn set_current_session(&mut self, session_id: Option<String>) {
        self.current_session_id = session_id;
    }

    pub fn update_activity_state(&mut self, activity_state: Option<SessionActivityState>) {
        self.activity_state = activity_state;
    }

    pub fn update_pending_message(&mut self, message: Option<String>) {
        self.pending_message = message;
    }

    pub fn update_session_activity_state(&mut self, session_id: String, activity_state: SessionActivityState) {
        self.session_activity_states.insert(session_id, activity_state);
    }

    #[allow(dead_code)]
    pub fn set_rate_limited(&mut self, seconds_remaining: u64) {
        self.rate_limited = true;
        self.rate_limit_seconds = seconds_remaining;
    }

    #[allow(dead_code)]
    pub fn clear_rate_limit(&mut self) {
        self.rate_limited = false;
        self.rate_limit_seconds = 0;
    }
}
