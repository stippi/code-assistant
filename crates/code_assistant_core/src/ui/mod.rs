pub mod streaming;
pub mod ui_events;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
pub use streaming::DisplayFragment;
pub use ui_events::UiEvent;

// Shared with the agent core: tool status and UI error vocabulary.
pub use agent_core::ui::{ToolStatus, UIError};

#[async_trait]
pub trait UserInterface: Send + Sync {
    /// Send an event to the UI
    async fn send_event(&self, event: UiEvent) -> Result<(), UIError>;

    /// Display a streaming fragment with specific type information
    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError>;

    /// Stream raw terminal output (ANSI bytes) for a tool's card.
    ///
    /// Unlike [`display_fragment`](Self::display_fragment), this bypasses
    /// the in-flight fragment buffer used for snapshots, so a background
    /// process can keep streaming to its terminal card between turns
    /// without growing that buffer unboundedly. Default no-op; only
    /// terminal-emulator frontends implement it.
    fn stream_terminal_output(&self, _tool_id: &str, _bytes: &[u8]) {}

    /// Signal that the process backing a tool's terminal exited. Like
    /// [`stream_terminal_output`](Self::stream_terminal_output) this bypasses
    /// the in-flight fragment buffer. Lets a terminal-emulator frontend mark
    /// the card finished (stop the spinner/stop button) even for a background
    /// session the agent never polls again. Default no-op.
    fn stream_terminal_exit(&self, _tool_id: &str, _exit_code: Option<i32>) {}

    /// Check if streaming should continue
    fn should_streaming_continue(&self) -> bool;

    /// Notify the UI about rate limiting and countdown
    fn notify_rate_limit(&self, seconds_remaining: u64);

    /// Clear rate limit notification
    fn clear_rate_limit(&self);

    /// The LLM request failed before anything was streamed and is sent
    /// again (`attempt` of `max_attempts`). Cleared by the next response.
    fn notify_retrying(&self, _reason: &str, _attempt: u32, _max_attempts: u32) {}

    /// Ask the user multiple-choice questions and wait for the answers
    /// (`ask_question` tool). Default: unsupported — only frontends with a
    /// human in the loop implement it.
    async fn ask_questions(
        &self,
        _request: crate::session::questions::UserQuestionRequest,
    ) -> Result<crate::session::questions::QuestionOutcome, UIError> {
        Err(UIError::IOError(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "This frontend cannot ask the user questions",
        )))
    }
}

/// Implements the agent core's UI boundary on top of a [`UserInterface`]:
/// loop events are translated into the application vocabulary
/// ([`UiEvent::from_agent`]) and stamped with the session id.
pub struct AgentUiAdapter {
    inner: Arc<dyn UserInterface>,
    session_id: Mutex<Option<String>>,
}

impl AgentUiAdapter {
    pub fn new(inner: Arc<dyn UserInterface>) -> Self {
        Self {
            inner,
            session_id: Mutex::new(None),
        }
    }

    pub fn set_session_id(&self, session_id: Option<String>) {
        *self.session_id.lock().unwrap() = session_id;
    }
}

#[async_trait]
impl agent_core::AgentUi for AgentUiAdapter {
    async fn send_event(&self, event: agent_core::AgentUiEvent) -> Result<(), UIError> {
        let session_id = self.session_id.lock().unwrap().clone();
        match UiEvent::from_agent(event, session_id.as_deref()) {
            Some(event) => self.inner.send_event(event).await,
            None => Ok(()),
        }
    }

    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError> {
        self.inner.display_fragment(fragment)
    }

    fn should_streaming_continue(&self) -> bool {
        self.inner.should_streaming_continue()
    }

    fn notify_rate_limit(&self, seconds_remaining: u64) {
        self.inner.notify_rate_limit(seconds_remaining);
    }

    fn clear_rate_limit(&self) {
        self.inner.clear_rate_limit();
    }

    fn notify_retrying(&self, reason: &str, attempt: u32, max_attempts: u32) {
        self.inner.notify_retrying(reason, attempt, max_attempts);
    }
}

/// The divider a stored compaction summary or new-context boundary renders
/// as, followed by the images a new context opens with.
pub fn context_divider(message: &llm::Message) -> Vec<DisplayFragment> {
    let summary = match &message.content {
        llm::MessageContent::Text(text) => text.trim().to_string(),
        llm::MessageContent::Structured(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                llm::ContentBlock::Text { text, .. } => Some(text.as_str()),
                llm::ContentBlock::Thinking { thinking, .. } => Some(thinking.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string(),
    };
    let divider = DisplayFragment::ContextDivider {
        boundary: agent_core::ui::ContextBoundary::of(message),
        summary,
    };
    let images = match &message.content {
        llm::MessageContent::Structured(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                llm::ContentBlock::Image {
                    media_type, data, ..
                } => Some(DisplayFragment::Image {
                    media_type: media_type.clone(),
                    data: data.clone(),
                }),
                _ => None,
            })
            .collect(),
        llm::MessageContent::Text(_) => Vec::new(),
    };
    std::iter::once(divider).chain(images).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_context_shows_its_images_below_the_divider() {
        let mut message = llm::Message::new_context(String::new());
        message.content = llm::MessageContent::Structured(vec![
            llm::ContentBlock::new_text("Next step"),
            llm::ContentBlock::Image {
                media_type: "image/png".into(),
                data: "aGk=".into(),
                start_time: None,
                end_time: None,
            },
        ]);

        let fragments = context_divider(&message);

        assert!(matches!(
            &fragments[..],
            [
                DisplayFragment::ContextDivider { summary, .. },
                DisplayFragment::Image { data, .. },
            ] if summary == "Next step" && data == "aGk="
        ));
    }
}
