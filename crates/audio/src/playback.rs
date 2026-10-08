//! The playback queue between the handle and the output callback.

use crate::{AudioReport, ReportSink};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct Playback {
    state: Mutex<State>,
    /// Samples handed to the device since the queue was created.
    played: AtomicU64,
    sink: ReportSink,
}

#[derive(Default)]
struct State {
    queue: VecDeque<f32>,
    /// Something was queued since the last drain report.
    playing: bool,
}

impl Playback {
    pub fn new(sink: ReportSink) -> Self {
        Self {
            state: Mutex::new(State::default()),
            played: AtomicU64::new(0),
            sink,
        }
    }

    pub fn push(&self, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }
        let mut state = self.state.lock().unwrap();
        state
            .queue
            .extend(samples.iter().map(|s| *s as f32 / i16::MAX as f32));
        state.playing = true;
    }

    /// Drop everything queued. No drain report follows: the caller stopped
    /// playback on purpose.
    pub fn clear(&self) {
        let mut state = self.state.lock().unwrap();
        state.queue.clear();
        state.playing = false;
    }

    pub fn played(&self) -> u64 {
        self.played.load(Ordering::Relaxed)
    }

    /// Whether audio is queued or was queued since the last drain.
    pub fn is_playing(&self) -> bool {
        self.state.lock().unwrap().playing
    }

    /// Fill a device buffer (24 kHz mono); silence once the queue is empty.
    /// Reports the drain when the last queued sample went out.
    pub fn render(&self, out: &mut [f32]) {
        let mut state = self.state.lock().unwrap();
        let available = state.queue.len().min(out.len());
        for (slot, sample) in out.iter_mut().zip(state.queue.drain(..available)) {
            *slot = sample;
        }
        out[available..].fill(0.0);
        self.played.fetch_add(available as u64, Ordering::Relaxed);
        let drained = state.playing && state.queue.is_empty();
        if drained {
            state.playing = false;
        }
        drop(state);
        if drained {
            (self.sink)(AudioReport::Drained);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn recording() -> (Playback, Arc<Mutex<Vec<AudioReport>>>) {
        let reports = Arc::new(Mutex::new(Vec::new()));
        let sink_reports = reports.clone();
        let playback = Playback::new(Arc::new(move |r| sink_reports.lock().unwrap().push(r)));
        (playback, reports)
    }

    #[test]
    fn counts_played_samples_and_reports_the_drain_once() {
        let (playback, reports) = recording();
        playback.push(&[i16::MAX; 300]);
        let mut buffer = [0.0; 256];
        playback.render(&mut buffer);
        assert_eq!(playback.played(), 256);
        assert!(reports.lock().unwrap().is_empty());
        playback.render(&mut buffer);
        assert_eq!(playback.played(), 300);
        assert!((buffer[43] - 1.0).abs() < 1e-6 && buffer[44] == 0.0);
        playback.render(&mut buffer);
        assert_eq!(*reports.lock().unwrap(), vec![AudioReport::Drained]);
    }

    #[test]
    fn clearing_drops_the_queue_without_a_drain_report() {
        let (playback, reports) = recording();
        playback.push(&[1; 1000]);
        playback.clear();
        let mut buffer = [1.0; 64];
        playback.render(&mut buffer);
        assert!(buffer.iter().all(|s| *s == 0.0));
        assert_eq!(playback.played(), 0);
        assert!(reports.lock().unwrap().is_empty());
        assert!(!playback.is_playing());
    }
}
