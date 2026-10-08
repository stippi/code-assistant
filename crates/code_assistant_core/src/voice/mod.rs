//! Voice mode: a global, speech-to-speech assistant over all conversations.
//!
//! The voice agent is a small agent loop of its own next to the sessions. It
//! talks to a realtime model (`llm::realtime`), streams microphone and
//! speaker audio through a [`VoiceAudio`] implementation the application
//! injects, and only has tools that act on conversations ([`tools`]). When a
//! conversation's agent finishes, the voice agent is notified — but never
//! while the user or the model speaks: [`floor::Floor`] decides when the
//! floor is free. See `docs/voice-mode.md`.
//!
//! Frontends drive it through [`VoiceService`] and follow it through the
//! app-scoped [`crate::ui::UiEvent::VoiceStatusChanged`] and
//! [`crate::ui::UiEvent::VoiceTranscript`] events.

mod agent;
pub mod config;
pub mod floor;
pub mod notifications;
mod service;
pub mod source;
pub mod tools;

#[cfg(test)]
mod tests;

pub use config::{NotifyScope, VoiceConfig};
pub use service::{ConnectorFactory, VoiceService, default_connector_factory};

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::mpsc;

/// What the audio devices report.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioEvent {
    /// Microphone samples: PCM16, 24 kHz, mono.
    Captured(Vec<i16>),
    /// The speakers played everything queued.
    Drained,
    /// The devices stopped working.
    Failed(String),
}

/// Microphone and speakers of a voice session. Implementations run the
/// devices on threads of their own and report through the
/// [`AudioEvent`] sender they were opened with.
pub trait VoiceAudio: Send {
    /// Queue samples (PCM16, 24 kHz, mono) for playback.
    fn play(&mut self, samples: Vec<i16>);
    /// Drop everything queued for playback.
    fn clear_playback(&mut self);
    /// Samples the speakers played since the devices opened.
    fn played_samples(&self) -> u64;
    /// Stop (or resume) reporting microphone samples.
    fn set_capture_muted(&mut self, muted: bool);
}

/// Opens the audio devices for a voice session.
pub type AudioFactory =
    Arc<dyn Fn(mpsc::UnboundedSender<AudioEvent>) -> Result<Box<dyn VoiceAudio>> + Send + Sync>;

/// What the voice agent is doing, for the frontends.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum VoiceActivity {
    #[default]
    Off,
    Connecting,
    /// Connected; the floor is free.
    Listening,
    /// The user has the floor.
    UserSpeaking,
    /// The model speaks (or prepares its answer).
    Speaking,
    /// Voice mode stopped because of this error.
    Failed(String),
}

impl VoiceActivity {
    pub fn is_active(&self) -> bool {
        !matches!(self, Self::Off | Self::Failed(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VoiceStatus {
    pub activity: VoiceActivity,
    pub muted: bool,
    /// Notifications waiting for a free floor.
    pub queued_notifications: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptRole {
    User,
    Assistant,
    /// A tool call the model made.
    Tool,
    /// A background notification delivered to the model.
    Notification,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    pub role: TranscriptRole,
    pub text: String,
}
