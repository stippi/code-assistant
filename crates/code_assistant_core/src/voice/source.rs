//! Turns the core's event stream into notifications for the voice agent.

use super::config::NotifyScope;
use super::floor::FloorInput;
use super::notifications::{Notification, NotificationKind};
use crate::session::SessionService;
use crate::session::event_stream::{EventPayload, SessionEvent};
use crate::session::instance::SessionActivityState;
use crate::ui::UiEvent;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// Permission and question requests open right now, shared with the tools
/// so `list_conversations` can tell which conversation waits for the user.
#[derive(Clone, Default)]
pub struct OpenRequests {
    /// request id → (conversation id, what it waits for)
    inner: Arc<Mutex<HashMap<String, (String, NotificationKind)>>>,
}

impl OpenRequests {
    fn open(&self, request_id: String, conversation_id: String, kind: NotificationKind) {
        self.inner
            .lock()
            .unwrap()
            .insert(request_id, (conversation_id, kind));
    }

    /// Close a request; returns its conversation when that has no other
    /// open request left.
    fn close(&self, request_id: &str) -> Option<String> {
        let mut inner = self.inner.lock().unwrap();
        let (conversation_id, _) = inner.remove(request_id)?;
        let still_waiting = inner.values().any(|(id, _)| *id == conversation_id);
        (!still_waiting).then_some(conversation_id)
    }

    fn close_conversation(&self, conversation_id: &str) {
        self.inner
            .lock()
            .unwrap()
            .retain(|_, (id, _)| id != conversation_id);
    }

    /// A state label when the conversation waits for the user.
    pub fn waiting_for(&self, conversation_id: &str) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        inner
            .values()
            .find(|(id, _)| id == conversation_id)
            .map(|(_, kind)| match kind {
                NotificationKind::NeedsPermission { tool } => {
                    format!("waiting for permission to run {tool}")
                }
                _ => "waiting for answers to its questions".to_string(),
            })
    }
}

/// What happened, before names are looked up.
#[derive(Debug, Clone, PartialEq)]
enum Change {
    Notify(String, NotificationKind),
    WaitEnded(String),
}

pub struct NotificationSource {
    service: SessionService,
    scope: NotifyScope,
    touched: HashSet<String>,
    states: HashMap<String, SessionActivityState>,
    open_requests: OpenRequests,
}

impl NotificationSource {
    /// Start from the current activity states, so a run that was going on
    /// before voice mode started is reported when it ends.
    pub async fn new(
        service: SessionService,
        scope: NotifyScope,
        open_requests: OpenRequests,
    ) -> Result<Self> {
        let states = service.session_activity_states().await?;
        Ok(Self {
            service,
            scope,
            touched: HashSet::new(),
            states,
            open_requests,
        })
    }

    /// The voice agent created or messaged this conversation.
    pub fn touch(&mut self, conversation_id: &str) {
        self.touched.insert(conversation_id.to_string());
    }

    pub async fn on_event(&mut self, event: &SessionEvent) -> Vec<FloorInput> {
        let Some(change) = self.change_for(event) else {
            return Vec::new();
        };
        self.resolve(vec![change]).await
    }

    /// After the stream lagged: compare the activity states with the last
    /// known ones.
    pub async fn resync(&mut self) -> Vec<FloorInput> {
        let Ok(current) = self.service.session_activity_states().await else {
            return Vec::new();
        };
        let mut changes = Vec::new();
        for (id, previous) in &self.states {
            let now = current.get(id).cloned().unwrap_or_default();
            if let Some(kind) = ended(previous, &now) {
                changes.push(Change::Notify(id.clone(), kind));
            }
        }
        self.states = current;
        let changes = changes
            .into_iter()
            .filter(|change| self.in_scope(change))
            .collect();
        self.resolve(changes).await
    }

    fn change_for(&mut self, event: &SessionEvent) -> Option<Change> {
        let EventPayload::Ui(ui) = &event.payload else {
            return None;
        };
        let session_id = event.session_id.clone();
        let change = match ui {
            UiEvent::UpdateSessionActivityState {
                session_id,
                activity_state,
            } => {
                let previous = self
                    .states
                    .insert(session_id.clone(), activity_state.clone());
                if activity_state.is_terminal() {
                    self.open_requests.close_conversation(session_id);
                }
                let kind = ended(&previous?, activity_state)?;
                Change::Notify(session_id.clone(), kind)
            }
            UiEvent::RequestToolPermission { request } => {
                let session_id = session_id?;
                let kind = NotificationKind::NeedsPermission {
                    tool: request.tool_name.clone(),
                };
                self.open_requests.open(
                    request.request_id.clone(),
                    session_id.clone(),
                    kind.clone(),
                );
                Change::Notify(session_id, kind)
            }
            UiEvent::RequestUserQuestions { request } => {
                let session_id = session_id?;
                let kind = NotificationKind::NeedsAnswer;
                self.open_requests.open(
                    request.request_id.clone(),
                    session_id.clone(),
                    kind.clone(),
                );
                Change::Notify(session_id, kind)
            }
            UiEvent::ToolPermissionRequestResolved { request_id }
            | UiEvent::UserQuestionsResolved { request_id } => {
                Change::WaitEnded(self.open_requests.close(request_id)?)
            }
            _ => return None,
        };
        self.in_scope(&change).then_some(change)
    }

    fn in_scope(&self, change: &Change) -> bool {
        match (self.scope, change) {
            (NotifyScope::All, _) | (_, Change::WaitEnded(_)) => true,
            (NotifyScope::Touched, Change::Notify(id, _)) => self.touched.contains(id),
        }
    }

    async fn resolve(&self, changes: Vec<Change>) -> Vec<FloorInput> {
        if changes.is_empty() {
            return Vec::new();
        }
        let sessions = self.service.list_sessions().await.unwrap_or_default();
        changes
            .into_iter()
            .map(|change| match change {
                Change::WaitEnded(conversation_id) => FloorInput::WaitEnded { conversation_id },
                Change::Notify(conversation_id, kind) => {
                    let metadata = super::tools::find_metadata(&sessions, &conversation_id);
                    FloorInput::Notification(Notification {
                        name: metadata
                            .map(|m| m.name.clone())
                            .unwrap_or_else(|| conversation_id.clone()),
                        project: metadata
                            .map(|m| m.initial_project.clone())
                            .unwrap_or_default(),
                        conversation_id,
                        kind,
                    })
                }
            })
            .collect()
    }
}

/// The notification for a run ending, when `previous → now` is one.
fn ended(previous: &SessionActivityState, now: &SessionActivityState) -> Option<NotificationKind> {
    let was_running = matches!(
        previous,
        SessionActivityState::AgentRunning
            | SessionActivityState::WaitingForResponse
            | SessionActivityState::RateLimited { .. }
    );
    if !was_running {
        return None;
    }
    match now {
        SessionActivityState::Idle => Some(NotificationKind::Finished),
        SessionActivityState::Errored { message } => Some(NotificationKind::Failed {
            message: message.clone(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_end_of_a_run_notifies() {
        use SessionActivityState as S;
        assert_eq!(
            ended(&S::AgentRunning, &S::Idle),
            Some(NotificationKind::Finished)
        );
        assert_eq!(
            ended(
                &S::WaitingForResponse,
                &S::Errored {
                    message: "x".into()
                }
            ),
            Some(NotificationKind::Failed {
                message: "x".into()
            })
        );
        assert_eq!(ended(&S::AgentRunning, &S::WaitingForResponse), None);
        // Clearing an error is not a run ending.
        assert_eq!(
            ended(
                &S::Errored {
                    message: "x".into()
                },
                &S::Idle
            ),
            None
        );
        assert_eq!(ended(&S::Idle, &S::Idle), None);
    }

    #[test]
    fn a_conversation_waits_until_its_last_request_closes() {
        let requests = OpenRequests::default();
        requests.open("r1".into(), "s".into(), NotificationKind::NeedsAnswer);
        requests.open(
            "r2".into(),
            "s".into(),
            NotificationKind::NeedsPermission { tool: "t".into() },
        );
        assert!(requests.waiting_for("s").is_some());
        assert_eq!(requests.close("r1"), None);
        assert_eq!(requests.close("r2"), Some("s".into()));
        assert_eq!(requests.waiting_for("s"), None);
        assert_eq!(requests.close("r2"), None);
    }
}
