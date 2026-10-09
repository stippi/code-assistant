//! The playback queue between the handle and the output callback.

use crate::{AudioReport, ReportSink};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The output callback runs on a real-time thread: it never waits for the
/// lock (it plays silence for one buffer instead), and the handle holds the
/// lock only to move a converted chunk in or out, never while it copies or
/// allocates sample data.
pub struct Playback {
    state: Mutex<State>,
    /// Something was queued since the last drain report.
    playing: AtomicBool,
    /// Samples handed to the device since the queue was created.
    played: AtomicU64,
    sink: ReportSink,
}

#[derive(Default)]
struct State {
    chunks: VecDeque<Vec<f32>>,
    /// Samples of the front chunk already played.
    offset: usize,
}

impl Playback {
    pub fn new(sink: ReportSink) -> Self {
        Self {
            state: Mutex::new(State::default()),
            playing: AtomicBool::new(false),
            played: AtomicU64::new(0),
            sink,
        }
    }

    pub fn push(&self, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }
        let chunk: Vec<f32> = samples
            .iter()
            .map(|s| *s as f32 / i16::MAX as f32)
            .collect();
        let mut state = self.state.lock().unwrap();
        state.chunks.push_back(chunk);
        self.playing.store(true, Ordering::Relaxed);
    }

    /// Drop everything queued. No drain report follows: the caller stopped
    /// playback on purpose.
    pub fn clear(&self) {
        let chunks = {
            let mut state = self.state.lock().unwrap();
            state.offset = 0;
            self.playing.store(false, Ordering::Relaxed);
            std::mem::take(&mut state.chunks)
        };
        drop(chunks);
    }

    pub fn played(&self) -> u64 {
        self.played.load(Ordering::Relaxed)
    }

    /// Whether audio is queued or was queued since the last drain.
    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    /// Fill a device buffer (24 kHz mono); silence once the queue is empty.
    /// Reports the drain when the last queued sample went out.
    pub fn render(&self, out: &mut [f32]) {
        let Ok(mut state) = self.state.try_lock() else {
            out.fill(0.0);
            return;
        };
        let mut written = 0;
        while written < out.len() {
            let offset = state.offset;
            let Some(chunk) = state.chunks.front() else {
                break;
            };
            let n = (chunk.len() - offset).min(out.len() - written);
            out[written..written + n].copy_from_slice(&chunk[offset..offset + n]);
            written += n;
            if offset + n == chunk.len() {
                // Freeing a chunk is cheap next to a missed deadline.
                state.chunks.pop_front();
                state.offset = 0;
            } else {
                state.offset = offset + n;
            }
        }
        out[written..].fill(0.0);
        self.played.fetch_add(written as u64, Ordering::Relaxed);
        let drained = state.chunks.is_empty() && self.playing.swap(false, Ordering::Relaxed);
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

    #[test]
    fn plays_across_chunks_in_order() {
        let (playback, reports) = recording();
        playback.push(&[i16::MAX; 3]);
        playback.push(&[0; 2]);
        playback.push(&[i16::MAX; 3]);
        let mut buffer = [9.0; 4];
        playback.render(&mut buffer);
        assert_eq!(buffer, [1.0, 1.0, 1.0, 0.0]);
        playback.render(&mut buffer);
        assert_eq!(buffer, [0.0, 1.0, 1.0, 1.0]);
        assert_eq!(playback.played(), 8);
        assert_eq!(*reports.lock().unwrap(), vec![AudioReport::Drained]);
    }

    #[test]
    fn a_held_lock_costs_one_silent_buffer_not_a_wait() {
        let (playback, _) = recording();
        playback.push(&[i16::MAX; 64]);
        let held = playback.state.lock().unwrap();
        let mut buffer = [1.0; 64];
        playback.render(&mut buffer);
        assert!(buffer.iter().all(|s| *s == 0.0));
        drop(held);
        playback.render(&mut buffer);
        assert_eq!(playback.played(), 64);
    }
}
