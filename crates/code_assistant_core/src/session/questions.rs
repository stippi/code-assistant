//! Session-layer question mediation for the `ask_question` tool: the agent
//! poses up to four multiple-choice questions, the request travels through
//! the broadcast [`EventStream`] to whatever frontend views the session, and
//! the answers come back via
//! [`crate::session::SessionService::answer_questions`].
//!
//! Mirrors the permission flow in [`crate::session::permissions`]: open
//! requests are included in session snapshots and are cancelled when the
//! user stops the agent or a new run starts.
//!
//! [`EventStream`]: crate::session::event_stream::EventStream

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::oneshot;

/// Maximum number of questions per request.
pub const MAX_QUESTIONS: usize = 4;
/// Minimum and maximum number of options per question.
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 4;

/// One selectable answer to a question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// One multiple-choice question posed by the agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserQuestion {
    pub question: String,
    /// Short label (a few words) frontends can show as a tab or chip.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub header: String,
    pub options: Vec<QuestionOption>,
    /// Checkboxes when true, radio buttons otherwise.
    #[serde(default)]
    pub multi_select: bool,
}

/// The user's answer to one question.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    /// Labels of the selected options, in option order.
    #[serde(default)]
    pub selected: Vec<String>,
    /// Free-text comment; may stand in for a selection ("other").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub comment: String,
}

/// A question request as shown to frontends. Carried by
/// [`crate::ui::UiEvent::RequestUserQuestions`] and included in session
/// snapshots so a frontend connecting mid-request can still answer it.
#[derive(Debug, Clone)]
pub struct UserQuestionRequest {
    /// Identifies the request towards `answer_questions`.
    pub request_id: String,
    /// The `ask_question` tool invocation this request belongs to.
    pub tool_id: Option<String>,
    pub questions: Vec<UserQuestion>,
}

impl UserQuestionRequest {
    pub fn new(tool_id: Option<String>, questions: Vec<UserQuestion>) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        Self {
            request_id: format!("question-{}", COUNTER.fetch_add(1, Ordering::Relaxed)),
            tool_id,
            questions,
        }
    }
}

/// How a question request ended.
#[derive(Debug, Clone, PartialEq)]
pub enum QuestionOutcome {
    /// One answer per question, in question order.
    Answered(Vec<QuestionAnswer>),
    /// The user dismissed the questions without answering.
    Declined,
    /// The request was dropped (agent stopped or a new run started).
    Cancelled,
}

/// Pending question requests of one session, keyed by request id.
///
/// Dropping an entry (e.g. `cancel_all` on stop) resolves the waiter as
/// [`QuestionOutcome::Cancelled`].
#[derive(Default)]
pub struct PendingQuestions {
    entries: Mutex<HashMap<String, PendingEntry>>,
}

struct PendingEntry {
    responder: oneshot::Sender<QuestionOutcome>,
    request: UserQuestionRequest,
}

impl PendingQuestions {
    pub(crate) fn insert(
        &self,
        request: UserQuestionRequest,
    ) -> oneshot::Receiver<QuestionOutcome> {
        let (tx, rx) = oneshot::channel();
        self.entries.lock().unwrap().insert(
            request.request_id.clone(),
            PendingEntry {
                responder: tx,
                request,
            },
        );
        rx
    }

    /// Feed the user's answers back to the waiting agent. Returns false if
    /// the request is unknown (already settled).
    pub fn resolve(&self, request_id: &str, outcome: QuestionOutcome) -> bool {
        match self.entries.lock().unwrap().remove(request_id) {
            Some(entry) => entry.responder.send(outcome).is_ok(),
            None => false,
        }
    }

    /// Drop all pending requests, resolving their waiters as cancelled.
    pub fn cancel_all(&self) {
        self.entries.lock().unwrap().clear();
    }

    /// The currently open requests, for session snapshots.
    pub fn snapshot(&self) -> Vec<UserQuestionRequest> {
        self.entries
            .lock()
            .unwrap()
            .values()
            .map(|entry| entry.request.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> UserQuestionRequest {
        UserQuestionRequest::new(
            None,
            vec![UserQuestion {
                question: "Which?".to_string(),
                header: String::new(),
                options: vec![],
                multi_select: false,
            }],
        )
    }

    #[tokio::test]
    async fn resolve_feeds_answers_to_waiter() {
        let pending = PendingQuestions::default();
        let request = request();
        let id = request.request_id.clone();
        let rx = pending.insert(request);
        let answers = vec![QuestionAnswer {
            selected: vec!["A".to_string()],
            comment: "because".to_string(),
        }];
        assert!(pending.resolve(&id, QuestionOutcome::Answered(answers.clone())));
        assert_eq!(rx.await.unwrap(), QuestionOutcome::Answered(answers));
        assert!(!pending.resolve(&id, QuestionOutcome::Declined));
    }

    #[tokio::test]
    async fn cancel_all_drops_waiters() {
        let pending = PendingQuestions::default();
        let rx = pending.insert(request());
        assert_eq!(pending.snapshot().len(), 1);
        pending.cancel_all();
        assert!(pending.snapshot().is_empty());
        assert!(rx.await.is_err());
    }
}
