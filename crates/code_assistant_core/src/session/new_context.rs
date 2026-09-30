//! Ending the current model context on purpose (see `docs/hand-off.md`):
//!
//! - `/new [prompt]` opens a fresh context whose first user message is
//!   `prompt`;
//! - `/hand-off [instruction]` (alias `/compact`) lets the agent write that
//!   prompt from the current context first.
//!
//! Both ask the user whether the new context continues behind a divider in
//! the same session or in a new session of the same project.

use crate::agent::Agent;
use crate::session::event_stream::EventStream;
use crate::ui::UiEvent;
use anyhow::Result;
use std::future::Future;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::oneshot;

const HANDOFF_PROMPT: &str = include_str!("../../resources/handoff_prompt.md");

/// What the `/hand-off` request asks the model to focus on. The user's
/// instruction is the typed text of the message the request is attached to.
const HANDOFF_FOCUS: &str = "The user asked for this hand-off by starting this message with \
`/hand-off`. The text after it, if any, says what the next context should work on or why the \
user hands off; follow it. Without such text, hand off the most obvious next step.";

/// A slash command that ends the current context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewContextCommand {
    /// `/new [prompt]`: open a new context with `prompt` (may be empty).
    New { prompt: String },
    /// `/hand-off [instruction]` or `/compact [instruction]`: the agent
    /// writes the prompt, steered by `instruction`.
    HandOff { instruction: String },
}

/// The built-in commands with their slash-menu descriptions, for frontends.
/// A skill of the same name is shadowed by these.
pub const COMMANDS: &[(&str, &str)] = &[
    ("new", "Start a new context with a prompt"),
    (
        "hand-off",
        "Let the agent write a hand-off prompt, then start a new context with it",
    ),
    ("compact", "Alias for /hand-off"),
];

/// Whether `name` (without the slash) is one of [`COMMANDS`].
pub fn is_command(name: &str) -> bool {
    COMMANDS.iter().any(|(command, _)| *command == name)
}

impl NewContextCommand {
    /// The command a message starts with, if any.
    pub fn parse(text: &str) -> Option<Self> {
        let (name, argument) = crate::skills::parse_skill_trigger(text)?;
        let argument = argument.to_string();
        match name {
            "new" => Some(Self::New { prompt: argument }),
            "hand-off" | "compact" => Some(Self::HandOff {
                instruction: argument,
            }),
            _ => None,
        }
    }
}

/// The hidden block appended to a `/hand-off` message: the request to write
/// the hand-off prompt (see [`crate::injection`]).
pub(crate) fn handoff_request() -> String {
    crate::injection::wrap(
        "hand-off-request",
        HANDOFF_PROMPT.replace("{focus}", HANDOFF_FOCUS).trim(),
    )
}

/// Where the new context continues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewContextTarget {
    /// Behind a divider in the same session.
    SameSession,
    /// In a new session of the same project.
    NewSession,
}

/// One answer to a [`NewContextTargetRequest`], in display order.
#[derive(Debug, Clone)]
pub struct NewContextTargetOption {
    pub target: NewContextTarget,
    pub label: String,
    pub description: String,
}

/// The question where a new context continues, as shown to frontends.
/// Carried by [`UiEvent::RequestNewContextTarget`] and included in session
/// snapshots so a frontend connecting mid-request can still answer it.
#[derive(Debug, Clone)]
pub struct NewContextTargetRequest {
    /// Identifies the request towards `respond_new_context_target`.
    pub request_id: String,
    pub options: Vec<NewContextTargetOption>,
}

impl NewContextTargetRequest {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        let option = |target, label: &str, description: &str| NewContextTargetOption {
            target,
            label: label.to_string(),
            description: description.to_string(),
        };
        Self {
            request_id: format!("new-context-{}", COUNTER.fetch_add(1, Ordering::Relaxed)),
            options: vec![
                option(
                    NewContextTarget::SameSession,
                    "This session",
                    "Continue here, behind a divider",
                ),
                option(
                    NewContextTarget::NewSession,
                    "New session",
                    "Continue in a new session of this project",
                ),
            ],
        }
    }
}

/// The open target question of a session, if any. Dropping it (stop
/// request, new run) cancels the command waiting for the answer.
#[derive(Default)]
pub struct PendingTargetRequest {
    slot: Mutex<Option<(NewContextTargetRequest, oneshot::Sender<NewContextTarget>)>>,
}

impl PendingTargetRequest {
    fn open(&self, request: NewContextTargetRequest) -> oneshot::Receiver<NewContextTarget> {
        let (tx, rx) = oneshot::channel();
        *self.slot.lock().unwrap() = Some((request, tx));
        rx
    }

    /// Feed the user's answer back. Returns false if the request is unknown
    /// (already answered or cancelled).
    pub fn resolve(&self, request_id: &str, target: NewContextTarget) -> bool {
        let mut slot = self.slot.lock().unwrap();
        if slot
            .as_ref()
            .is_none_or(|(request, _)| request.request_id != request_id)
        {
            return false;
        }
        let (_, responder) = slot.take().unwrap();
        responder.send(target).is_ok()
    }

    /// Drop the open request, cancelling the command waiting for it.
    pub fn cancel(&self) {
        self.slot.lock().unwrap().take();
    }

    /// The open request, for session snapshots.
    pub fn snapshot(&self) -> Option<NewContextTargetRequest> {
        self.slot
            .lock()
            .unwrap()
            .as_ref()
            .map(|(request, _)| request.clone())
    }
}

/// Ask the frontends viewing `session_id` where the new context continues.
/// Fails with [`tools_core::Cancelled`] when the request is dropped.
pub(crate) async fn ask_target(
    session_id: &str,
    events: &EventStream,
    pending: &PendingTargetRequest,
) -> Result<NewContextTarget> {
    let request = NewContextTargetRequest::new();
    let request_id = request.request_id.clone();
    let answer = pending.open(request.clone());
    events.publish_ui(session_id, UiEvent::RequestNewContextTarget { request });
    let answer = answer.await;
    // Settled either way: answered, or dropped by a stop request.
    pending.cancel();
    events.publish_ui(session_id, UiEvent::NewContextTargetResolved { request_id });
    answer.map_err(|_| tools_core::Cancelled.into())
}

/// A run that opens a new context instead of answering a user message.
pub(crate) struct NewContextRun {
    /// The prompt of `/new`; `None` for `/hand-off`, whose prompt the agent
    /// writes first (the request is the last message of the history).
    pub prompt: Option<String>,
    /// Receives the prompt when the user chose a new session, which the
    /// service then creates; the run itself only covers this session.
    pub new_session: oneshot::Sender<String>,
}

impl NewContextRun {
    /// Drive the run: get the prompt and the target concurrently, then open
    /// the new context here and continue from it, or hand the prompt over
    /// for a new session. A generated prompt is also recorded here as the
    /// answer to the `/hand-off` message.
    pub(crate) async fn run(
        self,
        agent: &mut Agent,
        target: impl Future<Output = Result<NewContextTarget>>,
    ) -> Result<()> {
        let generated = self.prompt.is_none();
        let (prompt, target) = match self.prompt {
            Some(prompt) => (prompt, target.await?),
            None => tokio::try_join!(agent.generate_handoff(None), target)?,
        };
        match target {
            NewContextTarget::SameSession => {
                let opens_with_prompt = !prompt.trim().is_empty();
                agent.append_new_context(prompt)?;
                if opens_with_prompt {
                    agent.run_single_iteration().await?;
                }
            }
            NewContextTarget::NewSession => {
                if generated {
                    agent.append_message(llm::Message::new_assistant(prompt.clone()))?;
                }
                // The service went away: nobody is left to create the session.
                let _ = self.new_session.send(prompt);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_commands() {
        assert_eq!(
            NewContextCommand::parse("/new write the tests"),
            Some(NewContextCommand::New {
                prompt: "write the tests".into()
            })
        );
        assert_eq!(
            NewContextCommand::parse("/new"),
            Some(NewContextCommand::New {
                prompt: String::new()
            })
        );
        assert_eq!(
            NewContextCommand::parse("  /hand-off\nfocus on docs\n"),
            Some(NewContextCommand::HandOff {
                instruction: "focus on docs".into()
            })
        );
        assert_eq!(
            NewContextCommand::parse("/compact"),
            Some(NewContextCommand::HandOff {
                instruction: String::new()
            })
        );
        assert_eq!(NewContextCommand::parse("/newer"), None);
        assert_eq!(NewContextCommand::parse("new"), None);
        assert!(is_command("hand-off") && !is_command("goal"));
    }

    #[test]
    fn handoff_request_is_a_hidden_block_with_the_focus() {
        let request = handoff_request();
        assert!(crate::injection::is_injection(&request));
        assert!(request.contains("`/hand-off`"));
        assert!(!request.contains("{focus}"));
    }

    #[tokio::test]
    async fn ask_target_returns_the_answer() {
        let events = EventStream::new();
        let pending = std::sync::Arc::new(PendingTargetRequest::default());
        let mut subscription = events.subscribe();

        let asking = {
            let events = events.clone();
            let pending = pending.clone();
            tokio::spawn(async move { ask_target("s1", &events, &pending).await })
        };
        let request_id = loop {
            let event = subscription.recv().await.unwrap();
            if let crate::session::event_stream::EventPayload::Ui(
                UiEvent::RequestNewContextTarget { request },
            ) = event.payload
            {
                break request.request_id;
            }
        };
        assert_eq!(
            pending.snapshot().map(|request| request.request_id),
            Some(request_id.clone())
        );
        assert!(!pending.resolve("other", NewContextTarget::NewSession));
        assert!(pending.resolve(&request_id, NewContextTarget::NewSession));

        assert_eq!(asking.await.unwrap().unwrap(), NewContextTarget::NewSession);
        assert!(pending.snapshot().is_none());
    }

    #[tokio::test]
    async fn ask_target_is_cancelled_when_the_request_is_dropped() {
        let events = EventStream::new();
        let pending = std::sync::Arc::new(PendingTargetRequest::default());
        let asking = {
            let events = events.clone();
            let pending = pending.clone();
            tokio::spawn(async move { ask_target("s1", &events, &pending).await })
        };
        while pending.snapshot().is_none() {
            tokio::task::yield_now().await;
        }
        pending.cancel();

        let error = asking.await.unwrap().unwrap_err();
        assert!(error.is::<tools_core::Cancelled>());
    }
}
