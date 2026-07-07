//! Session-layer permission mediation: routes a [`PermissionMediator`]
//! request from the agent through the broadcast [`EventStream`] to whatever
//! frontend views the session, and resolves it via
//! [`crate::session::SessionService::respond_permission`].

use crate::session::event_stream::EventStream;
use crate::ui::UiEvent;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use tools_core::permissions::{PermissionDecision, PermissionMediator, PermissionRequest};

/// A permission request as shown to frontends. Carried by
/// [`UiEvent::RequestToolPermission`] and included in session snapshots so a
/// frontend connecting mid-request can still answer it.
#[derive(Debug, Clone)]
pub struct ToolPermissionRequestData {
    /// Identifies the request towards `respond_permission`.
    pub request_id: String,
    /// The tool invocation this request belongs to, when known. Frontends
    /// can anchor the prompt at the tool's UI block.
    pub tool_id: Option<String>,
    pub tool_name: String,
    /// Human-readable description of what is being asked.
    pub summary: String,
    /// Structured request details (tool parameters or command line).
    pub metadata: serde_json::Value,
}

/// Pending permission requests of one session, keyed by request id.
///
/// The mediator inserts before publishing the UI event; `resolve` feeds the
/// user's decision back. Dropping an entry (e.g. `deny_all` on stop) resolves
/// the mediator side as `Denied`.
#[derive(Default)]
pub struct PendingPermissionRequests {
    entries: Mutex<HashMap<String, PendingEntry>>,
}

struct PendingEntry {
    responder: oneshot::Sender<PermissionDecision>,
    data: ToolPermissionRequestData,
}

impl PendingPermissionRequests {
    fn insert(&self, data: ToolPermissionRequestData) -> oneshot::Receiver<PermissionDecision> {
        let (tx, rx) = oneshot::channel();
        self.entries.lock().unwrap().insert(
            data.request_id.clone(),
            PendingEntry {
                responder: tx,
                data,
            },
        );
        rx
    }

    /// Feed the user's decision back to the waiting agent. Returns false if
    /// the request is unknown (already resolved or denied on stop).
    pub fn resolve(&self, request_id: &str, decision: PermissionDecision) -> bool {
        match self.entries.lock().unwrap().remove(request_id) {
            Some(entry) => entry.responder.send(decision).is_ok(),
            None => false,
        }
    }

    /// Drop all pending requests, resolving their mediators as `Denied`.
    /// Called when the user stops the agent and when a new run begins.
    pub fn deny_all(&self) {
        self.entries.lock().unwrap().clear();
    }

    /// The currently open requests, for session snapshots.
    pub fn snapshot(&self) -> Vec<ToolPermissionRequestData> {
        self.entries
            .lock()
            .unwrap()
            .values()
            .map(|entry| entry.data.clone())
            .collect()
    }
}

/// [`PermissionMediator`] for sessions driven through [`SessionService`]:
/// publishes the request on the event stream and awaits the decision from
/// `respond_permission`.
///
/// [`SessionService`]: crate::session::SessionService
pub struct SessionPermissionMediator {
    session_id: String,
    events: EventStream,
    pending: Arc<PendingPermissionRequests>,
}

impl SessionPermissionMediator {
    pub fn new(
        session_id: String,
        events: EventStream,
        pending: Arc<PendingPermissionRequests>,
    ) -> Self {
        Self {
            session_id,
            events,
            pending,
        }
    }

    fn next_request_id() -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        format!("perm-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    fn request_data(request: &PermissionRequest<'_>) -> ToolPermissionRequestData {
        use tools_core::permissions::PermissionRequestReason;

        let (summary, metadata) = match &request.reason {
            PermissionRequestReason::ExecuteCommand {
                command_line,
                working_dir,
            } => (
                match working_dir {
                    Some(dir) => format!("Run `{}` in {}", command_line, dir.display()),
                    None => format!("Run `{command_line}`"),
                },
                serde_json::json!({
                    "type": "execute_command",
                    "command_line": command_line,
                    "working_dir": working_dir.map(|dir| dir.display().to_string()),
                }),
            ),
            PermissionRequestReason::ToolInvocation { params } => (
                format!("Run tool `{}`", request.tool_name),
                serde_json::json!({
                    "type": "tool_invocation",
                    "params": params,
                }),
            ),
        };

        ToolPermissionRequestData {
            request_id: Self::next_request_id(),
            tool_id: request.tool_id.map(|id| id.to_string()),
            tool_name: request.tool_name.to_string(),
            summary,
            metadata,
        }
    }
}

#[async_trait]
impl PermissionMediator for SessionPermissionMediator {
    async fn request_permission(
        &self,
        request: PermissionRequest<'_>,
    ) -> Result<PermissionDecision> {
        let data = Self::request_data(&request);
        let request_id = data.request_id.clone();
        let rx = self.pending.insert(data.clone());

        self.events.publish_ui(
            &self.session_id,
            UiEvent::RequestToolPermission { request: data },
        );

        // A dropped responder (stop request, new agent run) counts as denial.
        let decision = rx.await.unwrap_or(PermissionDecision::Denied);

        // Tell every view the request is settled so open prompts dismiss.
        self.events.publish_ui(
            &self.session_id,
            UiEvent::ToolPermissionRequestResolved { request_id },
        );

        Ok(decision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(id: &str) -> ToolPermissionRequestData {
        ToolPermissionRequestData {
            request_id: id.to_string(),
            tool_id: None,
            tool_name: "edit".to_string(),
            summary: "Run tool `edit`".to_string(),
            metadata: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn resolve_feeds_decision_to_waiter() {
        let pending = PendingPermissionRequests::default();
        let rx = pending.insert(data("r1"));
        assert!(pending.resolve("r1", PermissionDecision::GrantedOnce));
        assert_eq!(rx.await.unwrap(), PermissionDecision::GrantedOnce);
        // Second resolve for the same id is a no-op.
        assert!(!pending.resolve("r1", PermissionDecision::Denied));
    }

    #[tokio::test]
    async fn deny_all_resolves_waiters_as_denied() {
        let pending = PendingPermissionRequests::default();
        let rx = pending.insert(data("r1"));
        assert_eq!(pending.snapshot().len(), 1);
        pending.deny_all();
        assert!(pending.snapshot().is_empty());
        // Sender dropped: the mediator maps this to Denied.
        assert!(rx.await.is_err());
    }
}
