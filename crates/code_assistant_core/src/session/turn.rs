//! Typed, correlation-safe turn tracking (the ROADMAP's "exact turn handle").
//!
//! [`SessionService::start_turn_if_idle`] is the atomic idle-only dispatch for
//! autonomous controllers. Unlike `try_send_user_message_if_idle`'s boolean it
//! returns a [`TurnHandle`] identifying the exact session and turn, which
//! resolves exactly once with a bounded [`TurnOutcome`] — the caller never
//! infers "its" turn from the broadcast event stream (a lagging subscriber
//! can drop transitions and mis-attribute completions).
//!
//! The [`TurnRecorder`] is the collection half: it is teed *synchronously*
//! into the session's event publisher, so it sees exactly what the agent
//! emitted for this run — narration fragments, tool lifecycle, resource
//! writes — without a lossy broadcast channel in between. The agent task
//! finishes the recorder when the run ends; aborting the task without a
//! finish resolves the outcome as failed via the recorder's `Drop`.
//!
//! [`SessionService::start_turn_if_idle`]: crate::session::SessionService::start_turn_if_idle

use crate::persistence::DraftAttachment;
use crate::tools::core::ToolScope;
use crate::ui::UiEvent;
use agent_core::ui::{DisplayFragment, ToolStatus};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// Everything the turn's visible narration may accumulate. Beyond this the
/// text is truncated — an outcome is a bounded summary, not a transcript.
const MAX_RESPONSE_LEN: usize = 64 * 1024;
/// Per-tool bound for the recorded output excerpt.
const MAX_TOOL_OUTPUT_LEN: usize = 8 * 1024;
/// Per-parameter bound for recorded tool parameter values.
const MAX_PARAMETER_LEN: usize = 2 * 1024;
/// How many tool invocations are recorded in detail. Further calls still
/// count in [`TurnUsage::tool_calls`].
const MAX_TOOL_RECORDS: usize = 256;
/// How many resource writes are recorded.
const MAX_RESOURCE_RECORDS: usize = 1024;

static NEXT_TURN_ID: AtomicU64 = AtomicU64::new(1);

/// What to run for a controller-started turn.
#[derive(Debug, Clone, Default)]
pub struct TurnRequest {
    pub message: String,
    pub attachments: Vec<DraftAttachment>,
    /// Per-run tool scope override, like `send_user_message_scoped`. `None`
    /// derives the scope from the session config as usual.
    pub tool_scope: Option<ToolScope>,
}

impl TurnRequest {
    pub fn text(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Default::default()
        }
    }

    pub fn with_tool_scope(mut self, scope: ToolScope) -> Self {
        self.tool_scope = Some(scope);
        self
    }
}

/// Result of the atomic idle-only dispatch.
pub enum TurnDispatch {
    /// The session is running another turn; nothing was appended or queued.
    Busy,
    /// The turn was started; the handle resolves when it ends.
    Started(TurnHandle),
}

/// How the turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnStatus {
    /// The agent loop finished normally.
    Completed,
    /// The run was stopped on request (`TurnHandle::cancel` / `request_stop`).
    Cancelled,
    /// The agent loop ended with an error, or the agent task died without
    /// reporting an outcome.
    Failed { error: String },
}

/// One recorded tool invocation of the turn (bounded).
#[derive(Debug, Clone)]
pub struct ToolRecord {
    pub tool_id: String,
    pub name: String,
    pub status: ToolStatus,
    /// The tool's short status message, when it reported one.
    pub message: Option<String>,
    /// Bounded excerpt of the tool output.
    pub output: Option<String>,
    /// Bounded parameter values in emission order (`replace` semantics of
    /// streamed parameters applied).
    pub parameters: Vec<(String, String)>,
}

/// A resource the turn wrote, as reported by the runtime's resource events.
#[derive(Debug, Clone)]
pub struct ResourceRef {
    pub project: String,
    pub path: PathBuf,
}

/// Bounded usage of the turn.
#[derive(Debug, Clone, Default)]
pub struct TurnUsage {
    /// LLM requests the turn made (streaming starts, including retries).
    pub llm_requests: u32,
    /// Tool invocations the turn made (including ones beyond the detailed
    /// record bound).
    pub tool_calls: u32,
    /// Wall time from dispatch to the terminal state.
    pub wall_time: Duration,
    /// Token usage of this turn: the session's total usage delta between
    /// turn start and the last persisted state. `None` when no state was
    /// persisted during the run (e.g. immediate failure).
    pub tokens: Option<llm::Usage>,
}

/// The bounded, typed result of one turn. Resolves exactly once per handle.
#[derive(Debug, Clone)]
pub struct TurnOutcome {
    pub turn_id: u64,
    pub status: TurnStatus,
    /// The assistant's visible narration of the turn (plain-text fragments;
    /// thinking is deliberately excluded), trimmed and bounded.
    pub final_response: String,
    /// Tool lifecycle records, bounded (see [`TurnUsage::tool_calls`] for
    /// the full count).
    pub tools: Vec<ToolRecord>,
    /// Resources written during the turn.
    pub resources_written: Vec<ResourceRef>,
    /// A queued user message was absorbed into this turn while it ran — the
    /// narration past that point answers the user, not only the controller.
    pub user_preempted: bool,
    pub usage: TurnUsage,
}

/// Identifies one started turn and resolves once with its outcome.
pub struct TurnHandle {
    session_id: String,
    turn_id: u64,
    service: crate::session::SessionService,
    outcome: oneshot::Receiver<TurnOutcome>,
}

impl TurnHandle {
    pub(crate) fn new(
        session_id: String,
        turn_id: u64,
        service: crate::session::SessionService,
        outcome: oneshot::Receiver<TurnOutcome>,
    ) -> Self {
        Self {
            session_id,
            turn_id,
            service,
            outcome,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn turn_id(&self) -> u64 {
        self.turn_id
    }

    /// Resolve the turn's outcome. A turn that errored still resolves (with
    /// [`TurnStatus::Failed`]); `Err` means the outcome itself was lost,
    /// which does not happen in an intact process.
    pub async fn wait(self) -> Result<TurnOutcome> {
        self.outcome
            .await
            .map_err(|_| anyhow::anyhow!("turn outcome lost (agent task dropped its recorder)"))
    }

    /// Ask the running agent to stop at its next checkpoint. The outcome
    /// still resolves (normally as [`TurnStatus::Cancelled`]).
    pub async fn cancel(&self) -> Result<()> {
        self.service.request_stop(self.session_id.clone()).await
    }
}

/// Collection half of a turn: teed synchronously into the session's event
/// publisher for the duration of one agent run. Constructed only by
/// [`SessionService::start_turn_if_idle`]; public because it appears in the
/// manager's and instance's (semi-internal) signatures.
///
/// [`SessionService::start_turn_if_idle`]: crate::session::SessionService::start_turn_if_idle
pub struct TurnRecorder {
    turn_id: u64,
    started: Instant,
    inner: Mutex<RecorderInner>,
}

struct RecorderInner {
    sender: Option<oneshot::Sender<TurnOutcome>>,
    response: String,
    tools: Vec<ToolRecord>,
    resources: Vec<ResourceRef>,
    user_preempted: bool,
    llm_requests: u32,
    tool_calls: u32,
    cancelled: bool,
    /// The session's total usage when the turn was armed.
    baseline_usage: llm::Usage,
    /// The session's total usage after the run (supplied at finish).
    latest_usage: Option<llm::Usage>,
}

impl TurnRecorder {
    /// Arm a recorder for a new turn. `baseline_usage` is the session's
    /// total usage before the turn (for the token delta).
    pub(crate) fn arm(baseline_usage: llm::Usage) -> (std::sync::Arc<Self>, TurnParts) {
        let (tx, rx) = oneshot::channel();
        let turn_id = NEXT_TURN_ID.fetch_add(1, Ordering::Relaxed);
        let recorder = std::sync::Arc::new(Self {
            turn_id,
            started: Instant::now(),
            inner: Mutex::new(RecorderInner {
                sender: Some(tx),
                response: String::new(),
                tools: Vec::new(),
                resources: Vec::new(),
                user_preempted: false,
                llm_requests: 0,
                tool_calls: 0,
                cancelled: false,
                baseline_usage,
                latest_usage: None,
            }),
        });
        (
            recorder,
            TurnParts {
                turn_id,
                outcome: rx,
            },
        )
    }

    /// Tee of the publisher's `send_event`.
    pub(crate) fn observe(&self, event: &UiEvent) {
        let mut inner = self.inner.lock().expect("turn recorder lock poisoned");
        match event {
            UiEvent::StreamingStarted { .. } => inner.llm_requests += 1,
            UiEvent::StreamingStopped {
                cancelled: true, ..
            } => inner.cancelled = true,
            // Published by the agent when it absorbs a queued user message
            // mid-run (the turn's own user message is announced by the
            // service before the run, not through the run's publisher).
            UiEvent::DisplayUserInput { .. } => inner.user_preempted = true,
            UiEvent::StartTool { name, id } => {
                inner.tool_calls += 1;
                if inner.tools.len() < MAX_TOOL_RECORDS {
                    inner.tools.push(ToolRecord {
                        tool_id: id.clone(),
                        name: name.clone(),
                        status: ToolStatus::Pending,
                        message: None,
                        output: None,
                        parameters: Vec::new(),
                    });
                }
            }
            UiEvent::UpdateToolParameter {
                tool_id,
                name,
                value,
                replace,
            } => {
                if let Some(tool) = inner.tools.iter_mut().find(|t| &t.tool_id == tool_id) {
                    match tool.parameters.iter_mut().find(|(n, _)| n == name) {
                        Some((_, existing)) => {
                            if *replace {
                                existing.clear();
                            }
                            push_bounded(existing, value, MAX_PARAMETER_LEN);
                        }
                        None => {
                            let mut recorded = String::new();
                            push_bounded(&mut recorded, value, MAX_PARAMETER_LEN);
                            tool.parameters.push((name.clone(), recorded));
                        }
                    }
                }
            }
            UiEvent::UpdateToolStatus {
                tool_id,
                status,
                message,
                output,
                ..
            } => {
                if let Some(tool) = inner.tools.iter_mut().find(|t| &t.tool_id == tool_id) {
                    tool.status = *status;
                    if message.is_some() {
                        tool.message = message.clone();
                    }
                    if let Some(output) = output {
                        let recorded = tool.output.get_or_insert_with(String::new);
                        recorded.clear();
                        push_bounded(recorded, output, MAX_TOOL_OUTPUT_LEN);
                    }
                }
            }
            UiEvent::ResourceWritten { project, path } => {
                if inner.resources.len() < MAX_RESOURCE_RECORDS {
                    inner.resources.push(ResourceRef {
                        project: project.clone(),
                        path: path.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    /// Tee of the publisher's `display_fragment`. Only visible narration is
    /// recorded — thinking is never part of an outcome.
    pub(crate) fn observe_fragment(&self, fragment: &DisplayFragment) {
        if let DisplayFragment::PlainText(text) = fragment {
            let mut inner = self.inner.lock().expect("turn recorder lock poisoned");
            let response = &mut inner.response;
            push_bounded(response, text, MAX_RESPONSE_LEN);
        }
    }

    /// Resolve the outcome. `error` is the agent task's terminal error, if
    /// any; a stop request recorded during the run wins over `Completed`.
    /// `final_total_usage` is the session's total usage after the run (read
    /// from the persisted state, which the agent saves synchronously) — the
    /// outcome's token usage is its delta against the armed baseline.
    pub(crate) fn finish(&self, error: Option<String>, final_total_usage: Option<llm::Usage>) {
        let mut inner = self.inner.lock().expect("turn recorder lock poisoned");
        inner.latest_usage = final_total_usage.or(inner.latest_usage.take());
        let Some(sender) = inner.sender.take() else {
            return;
        };
        let status = match error {
            Some(error) => TurnStatus::Failed { error },
            None if inner.cancelled => TurnStatus::Cancelled,
            None => TurnStatus::Completed,
        };
        let tokens = inner.latest_usage.as_ref().map(|latest| llm::Usage {
            input_tokens: latest
                .input_tokens
                .saturating_sub(inner.baseline_usage.input_tokens),
            output_tokens: latest
                .output_tokens
                .saturating_sub(inner.baseline_usage.output_tokens),
            cache_creation_input_tokens: latest
                .cache_creation_input_tokens
                .saturating_sub(inner.baseline_usage.cache_creation_input_tokens),
            cache_read_input_tokens: latest
                .cache_read_input_tokens
                .saturating_sub(inner.baseline_usage.cache_read_input_tokens),
        });
        let outcome = TurnOutcome {
            turn_id: self.turn_id,
            status,
            final_response: inner.response.trim().to_string(),
            tools: std::mem::take(&mut inner.tools),
            resources_written: std::mem::take(&mut inner.resources),
            user_preempted: inner.user_preempted,
            usage: TurnUsage {
                llm_requests: inner.llm_requests,
                tool_calls: inner.tool_calls,
                wall_time: self.started.elapsed(),
                tokens,
            },
        };
        // The handle may already be gone (caller dropped it) — fine.
        let _ = sender.send(outcome);
    }
}

impl Drop for TurnRecorder {
    /// An aborted agent task (e.g. `terminate_agent`) never reaches its
    /// finish; resolve the outcome as failed instead of losing it.
    fn drop(&mut self) {
        if self
            .inner
            .lock()
            .map(|inner| inner.sender.is_some())
            .unwrap_or(false)
        {
            self.finish(
                Some("agent task ended without reporting an outcome".to_string()),
                None,
            );
        }
    }
}

/// The handle-side parts produced by [`TurnRecorder::arm`].
pub(crate) struct TurnParts {
    pub(crate) turn_id: u64,
    pub(crate) outcome: oneshot::Receiver<TurnOutcome>,
}

fn push_bounded(target: &mut String, addition: &str, bound: usize) {
    let remaining = bound.saturating_sub(target.len());
    if remaining == 0 {
        return;
    }
    if addition.len() <= remaining {
        target.push_str(addition);
    } else {
        let mut cut = remaining;
        while !addition.is_char_boundary(cut) {
            cut -= 1;
        }
        target.push_str(&addition[..cut]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn armed() -> (std::sync::Arc<TurnRecorder>, oneshot::Receiver<TurnOutcome>) {
        let (recorder, parts) = TurnRecorder::arm(llm::Usage::zero());
        (recorder, parts.outcome)
    }

    fn usage(input: u32, output: u32) -> llm::Usage {
        llm::Usage {
            input_tokens: input,
            output_tokens: output,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        }
    }

    #[test]
    fn records_narration_tools_and_resources() {
        let (recorder, outcome) = armed();

        recorder.observe(&UiEvent::StreamingStarted {
            request_id: 1,
            node_id: 1,
        });
        recorder.observe_fragment(&DisplayFragment::PlainText("Working on it. ".into()));
        recorder.observe_fragment(&DisplayFragment::ThinkingText {
            text: "secret reasoning".into(),
            duration_seconds: None,
        });
        recorder.observe(&UiEvent::StartTool {
            name: "write_file".into(),
            id: "t1".into(),
        });
        recorder.observe(&UiEvent::UpdateToolParameter {
            tool_id: "t1".into(),
            name: "path".into(),
            value: "notes.md".into(),
            replace: false,
        });
        recorder.observe(&UiEvent::UpdateToolStatus {
            tool_id: "t1".into(),
            status: ToolStatus::Success,
            message: Some("wrote notes.md".into()),
            output: Some("ok".into()),
            styled_output: None,
            duration_seconds: None,
            images: Vec::new(),
        });
        recorder.observe(&UiEvent::ResourceWritten {
            project: "workspace".into(),
            path: PathBuf::from("notes.md"),
        });
        recorder.observe_fragment(&DisplayFragment::PlainText("Done.".into()));
        recorder.finish(None, None);

        let outcome = outcome.blocking_recv().unwrap();
        assert_eq!(outcome.status, TurnStatus::Completed);
        assert_eq!(outcome.final_response, "Working on it. Done.");
        assert!(!outcome.final_response.contains("secret"));
        assert_eq!(outcome.tools.len(), 1);
        assert_eq!(outcome.tools[0].name, "write_file");
        assert_eq!(outcome.tools[0].status, ToolStatus::Success);
        assert_eq!(
            outcome.tools[0].parameters,
            vec![("path".to_string(), "notes.md".to_string())]
        );
        assert_eq!(outcome.resources_written.len(), 1);
        assert_eq!(outcome.usage.llm_requests, 1);
        assert_eq!(outcome.usage.tool_calls, 1);
        assert!(!outcome.user_preempted);
    }

    #[test]
    fn tokens_are_the_delta_against_the_baseline() {
        let (recorder, parts) = TurnRecorder::arm(usage(100, 50));
        recorder.finish(None, Some(usage(160, 80)));
        let outcome = parts.outcome.blocking_recv().unwrap();
        let tokens = outcome.usage.tokens.expect("delta recorded");
        assert_eq!(tokens.input_tokens, 60);
        assert_eq!(tokens.output_tokens, 30);
    }

    #[test]
    fn a_stop_request_resolves_as_cancelled_and_an_error_as_failed() {
        let (recorder, outcome) = armed();
        recorder.observe(&UiEvent::StreamingStopped {
            id: 1,
            cancelled: true,
            error: None,
        });
        recorder.finish(None, None);
        assert_eq!(
            outcome.blocking_recv().unwrap().status,
            TurnStatus::Cancelled
        );

        let (recorder, outcome) = armed();
        recorder.finish(Some("model exploded".into()), None);
        assert_eq!(
            outcome.blocking_recv().unwrap().status,
            TurnStatus::Failed {
                error: "model exploded".into()
            }
        );
    }

    #[test]
    fn an_absorbed_user_message_marks_preemption() {
        let (recorder, outcome) = armed();
        recorder.observe(&UiEvent::DisplayUserInput {
            content: "actually, stop".into(),
            attachments: Vec::new(),
            node_id: None,
        });
        recorder.finish(None, None);
        assert!(outcome.blocking_recv().unwrap().user_preempted);
    }

    #[test]
    fn dropping_an_unfinished_recorder_fails_the_outcome_instead_of_losing_it() {
        let (recorder, outcome) = armed();
        drop(recorder);
        match outcome.blocking_recv().unwrap().status {
            TurnStatus::Failed { error } => assert!(error.contains("without reporting")),
            status => panic!("expected Failed, got {status:?}"),
        }
    }

    #[test]
    fn narration_and_tool_output_are_bounded() {
        let (recorder, outcome) = armed();
        let chunk = "x".repeat(50 * 1024);
        recorder.observe_fragment(&DisplayFragment::PlainText(chunk.clone()));
        recorder.observe_fragment(&DisplayFragment::PlainText(chunk.clone()));
        recorder.observe(&UiEvent::StartTool {
            name: "execute_command".into(),
            id: "t1".into(),
        });
        recorder.observe(&UiEvent::UpdateToolStatus {
            tool_id: "t1".into(),
            status: ToolStatus::Success,
            message: None,
            output: Some(chunk),
            styled_output: None,
            duration_seconds: None,
            images: Vec::new(),
        });
        recorder.finish(None, None);
        let outcome = outcome.blocking_recv().unwrap();
        assert_eq!(outcome.final_response.len(), MAX_RESPONSE_LEN);
        assert_eq!(
            outcome.tools[0].output.as_ref().unwrap().len(),
            MAX_TOOL_OUTPUT_LEN
        );
    }
}
