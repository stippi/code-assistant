//! Voice mode state mirrored from the core, and the commands to drive it.

use crate::Gpui;
use code_assistant_core::voice::{TranscriptEntry, VoiceConfig, VoiceService, VoiceStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Transcript lines kept for the title-bar popover.
const TRANSCRIPT_LIMIT: usize = 200;

#[derive(Clone, Default)]
pub struct VoiceState {
    service: Arc<Mutex<Option<VoiceService>>>,
    status: Arc<Mutex<VoiceStatus>>,
    transcript: Arc<Mutex<Vec<TranscriptEntry>>>,
    /// Whether `voice.json` names a provider and model.
    configured: Arc<AtomicBool>,
}

impl Gpui {
    /// Install the voice service; without one the title bar shows no voice
    /// control.
    pub fn set_voice_service(&self, service: VoiceService) {
        *self.voice.service.lock().unwrap() = Some(service);
        self.refresh_voice_configured();
    }

    pub fn voice_available(&self) -> bool {
        self.voice.service.lock().unwrap().is_some()
    }

    /// Whether a voice provider and model are configured.
    pub fn voice_configured(&self) -> bool {
        self.voice.configured.load(Ordering::Relaxed)
    }

    /// Re-read `voice.json` (after the settings or the file changed).
    pub fn refresh_voice_configured(&self) {
        self.voice
            .configured
            .store(VoiceConfig::load().is_configured(), Ordering::Relaxed);
    }

    pub fn voice_status(&self) -> VoiceStatus {
        self.voice.status.lock().unwrap().clone()
    }

    pub fn voice_transcript(&self) -> Vec<TranscriptEntry> {
        self.voice.transcript.lock().unwrap().clone()
    }

    /// Start voice mode, or stop it when it runs.
    pub fn toggle_voice(&self) {
        let Some(service) = self.voice.service.lock().unwrap().clone() else {
            return;
        };
        if self.voice_status().activity.is_active() {
            service.stop();
        } else {
            self.voice.transcript.lock().unwrap().clear();
            service.start();
        }
    }

    pub fn set_voice_muted(&self, muted: bool) {
        if let Some(service) = self.voice.service.lock().unwrap().as_ref() {
            service.set_muted(muted);
        }
    }

    pub(crate) fn apply_voice_status(&self, status: VoiceStatus) {
        *self.voice.status.lock().unwrap() = status;
    }

    pub(crate) fn append_voice_transcript(&self, entry: TranscriptEntry) {
        let mut transcript = self.voice.transcript.lock().unwrap();
        transcript.push(entry);
        let excess = transcript.len().saturating_sub(TRANSCRIPT_LIMIT);
        transcript.drain(..excess);
    }
}
