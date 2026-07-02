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

    /// Check if streaming should continue
    fn should_streaming_continue(&self) -> bool;

    /// Notify the UI about rate limiting and countdown
    fn notify_rate_limit(&self, seconds_remaining: u64);

    /// Clear rate limit notification
    fn clear_rate_limit(&self);

    /// Downcast to Any for accessing concrete type methods
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Transitional no-op [`UserInterface`] for frontends that consume the
/// broadcast event stream instead of the legacy push path. Removed together
/// with that path once all frontends have migrated.
pub struct NullUserInterface;

#[async_trait]
impl UserInterface for NullUserInterface {
    async fn send_event(&self, _event: UiEvent) -> Result<(), UIError> {
        Ok(())
    }

    fn display_fragment(&self, _fragment: &DisplayFragment) -> Result<(), UIError> {
        Ok(())
    }

    fn should_streaming_continue(&self) -> bool {
        true
    }

    fn notify_rate_limit(&self, _seconds_remaining: u64) {}

    fn clear_rate_limit(&self) {}

    fn as_any(&self) -> &dyn std::any::Any {
        self
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
}
