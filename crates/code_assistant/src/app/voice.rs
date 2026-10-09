//! The audio devices of voice mode, from the `audio` crate.

use code_assistant_core::voice::{AudioEvent, AudioFactory, VoiceAudio};
use std::sync::Arc;

struct Devices(audio::AudioIo);

impl VoiceAudio for Devices {
    fn play(&mut self, samples: Vec<i16>) {
        self.0.play(&samples);
    }

    fn clear_playback(&mut self) {
        self.0.clear_playback();
    }

    fn played_samples(&self) -> u64 {
        self.0.played_samples()
    }

    fn set_capture_muted(&mut self, muted: bool) {
        self.0.set_muted(muted);
    }
}

/// Opens the default microphone and speakers for each voice session.
pub fn audio_factory() -> AudioFactory {
    Arc::new(|events| {
        let sink: audio::ReportSink = Arc::new(move |report| {
            let event = match report {
                audio::AudioReport::Captured(samples) => AudioEvent::Captured(samples),
                audio::AudioReport::Drained => AudioEvent::Drained,
                audio::AudioReport::Failed(message) => AudioEvent::Failed(message),
            };
            let _ = events.send(event);
        });
        Ok(Box::new(Devices(audio::AudioIo::open(sink)?)) as Box<dyn VoiceAudio>)
    })
}
