//! The core→UI broadcast stream.
//!
//! Everything the core wants frontends to know — streaming fragments,
//! state changes, notifications — is *published* here, tagged with the
//! session it belongs to. Frontends [`subscribe`](EventStream::subscribe)
//! and decide themselves what to render; the core does not know which
//! session is being viewed (or how many views exist).
//!
//! Delivery is best-effort with bounded buffering: a subscriber that falls
//! behind observes [`StreamError::Lagged`] and is expected to resync by
//! calling `SessionService::load_session` for a fresh snapshot, then
//! continue consuming.

use crate::ui::{DisplayFragment, UiEvent};
use tokio::sync::broadcast;

/// One item on the core→UI stream.
#[derive(Debug, Clone)]
pub struct SessionEvent {
    /// The session this event belongs to; `None` for app-scoped events
    /// (chat list updates, config changes).
    pub session_id: Option<String>,
    pub payload: EventPayload,
}

#[derive(Debug, Clone)]
pub enum EventPayload {
    /// A streaming display fragment of a session's in-flight assistant
    /// response.
    Fragment(DisplayFragment),
    /// An application notification.
    Ui(UiEvent),
}

/// Why a subscription stopped yielding events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamError {
    /// The subscriber fell behind and `missed` events were dropped. The
    /// subscription is still usable; resync via a fresh session snapshot.
    Lagged { missed: u64 },
    /// The core shut down; no more events will arrive.
    Closed,
}

/// Cloneable publisher handle for the core→UI stream.
#[derive(Clone)]
pub struct EventStream {
    sender: broadcast::Sender<SessionEvent>,
}

/// Number of events buffered per lagging subscriber before drops occur.
/// Streaming produces many small fragment events, so this is sized
/// generously; a lagged subscriber resyncs via snapshot, so drops are
/// recoverable.
const CHANNEL_CAPACITY: usize = 4096;

impl Default for EventStream {
    fn default() -> Self {
        Self::new()
    }
}

impl EventStream {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self { sender }
    }

    pub fn subscribe(&self) -> Subscription {
        Subscription {
            receiver: self.sender.subscribe(),
        }
    }

    /// Publish an event. Never blocks; if no subscriber exists the event is
    /// dropped (frontends resync via snapshot when they attach).
    pub fn publish(&self, session_id: Option<String>, payload: EventPayload) {
        let _ = self.sender.send(SessionEvent {
            session_id,
            payload,
        });
    }

    /// Publish a session-scoped notification.
    pub fn publish_ui(&self, session_id: impl Into<String>, event: UiEvent) {
        self.publish(Some(session_id.into()), EventPayload::Ui(event));
    }

    /// Publish an app-scoped notification.
    pub fn publish_app(&self, event: UiEvent) {
        self.publish(None, EventPayload::Ui(event));
    }
}

/// A frontend's subscription to the core→UI stream.
pub struct Subscription {
    receiver: broadcast::Receiver<SessionEvent>,
}

impl Subscription {
    /// Wait for the next event.
    pub async fn recv(&mut self) -> Result<SessionEvent, StreamError> {
        match self.receiver.recv().await {
            Ok(event) => Ok(event),
            Err(broadcast::error::RecvError::Lagged(missed)) => Err(StreamError::Lagged { missed }),
            Err(broadcast::error::RecvError::Closed) => Err(StreamError::Closed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn events_reach_all_subscribers() {
        let stream = EventStream::new();
        let mut a = stream.subscribe();
        let mut b = stream.subscribe();

        stream.publish_ui("s1", UiEvent::ClearMessages);

        for subscription in [&mut a, &mut b] {
            let event = subscription.recv().await.unwrap();
            assert_eq!(event.session_id.as_deref(), Some("s1"));
            assert!(matches!(
                event.payload,
                EventPayload::Ui(UiEvent::ClearMessages)
            ));
        }
    }

    #[tokio::test]
    async fn app_scoped_events_have_no_session() {
        let stream = EventStream::new();
        let mut sub = stream.subscribe();
        stream.publish_app(UiEvent::ConfigChanged);
        assert_eq!(sub.recv().await.unwrap().session_id, None);
    }

    #[tokio::test]
    async fn fragments_are_session_tagged() {
        let stream = EventStream::new();
        let mut sub = stream.subscribe();
        stream.publish(
            Some("s1".to_string()),
            EventPayload::Fragment(DisplayFragment::PlainText("hi".to_string())),
        );
        let event = sub.recv().await.unwrap();
        assert_eq!(event.session_id.as_deref(), Some("s1"));
        assert!(matches!(
            event.payload,
            EventPayload::Fragment(DisplayFragment::PlainText(ref t)) if t == "hi"
        ));
    }

    #[tokio::test]
    async fn publish_without_subscribers_is_a_noop() {
        let stream = EventStream::new();
        stream.publish_app(UiEvent::ConfigChanged);
        // A later subscriber does not see earlier events…
        let mut sub = stream.subscribe();
        stream.publish_app(UiEvent::ClearError);
        // …only the ones published after subscribing.
        assert!(matches!(
            sub.recv().await.unwrap().payload,
            EventPayload::Ui(UiEvent::ClearError)
        ));
    }

    #[tokio::test]
    async fn slow_subscriber_observes_lag_and_can_continue() {
        let stream = EventStream::new();
        let mut sub = stream.subscribe();

        // Overflow the per-subscriber buffer.
        for _ in 0..(CHANNEL_CAPACITY + 10) {
            stream.publish_app(UiEvent::ConfigChanged);
        }

        match sub.recv().await {
            Err(StreamError::Lagged { missed }) => assert!(missed >= 10),
            other => panic!("expected lag, got {other:?}"),
        }
        // The subscription keeps working after the lag signal.
        assert!(sub.recv().await.is_ok());
    }

    #[tokio::test]
    async fn closed_stream_reports_closed() {
        let stream = EventStream::new();
        let mut sub = stream.subscribe();
        drop(stream);
        assert!(matches!(sub.recv().await, Err(StreamError::Closed)));
    }
}
