//! Microphone capture and speaker playback for voice conversations.
//!
//! Audio is PCM16 mono at 24 kHz in both directions, the format of realtime
//! speech models. [`AudioIo::open`] starts the default devices on a thread of
//! their own; the handle queues playback and reports through a callback:
//! captured microphone chunks, the moment the speakers drained, and device
//! failures.
//!
//! On macOS the devices run through the voice-processing I/O unit, whose
//! echo cancellation keeps the speakers out of the microphone, so the user
//! can talk over the model. Elsewhere `cpal` streams are used and the
//! microphone is gated while audio plays (half duplex: no barge-in).

#[cfg(not(target_os = "macos"))]
mod cpal_backend;
#[cfg(target_os = "macos")]
mod macos;
mod playback;
#[cfg(any(test, not(target_os = "macos")))]
mod resample;

use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;

pub use playback::Playback;

/// Sample rate of the audio this crate exchanges.
pub const SAMPLE_RATE: u32 = 24_000;
/// Samples per captured chunk (50 ms).
const CAPTURE_CHUNK: usize = 1_200;

/// What the devices report.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioReport {
    /// Microphone samples.
    Captured(Vec<i16>),
    /// The speakers played everything queued.
    Drained,
    /// The devices stopped working.
    Failed(String),
}

pub type ReportSink = Arc<dyn Fn(AudioReport) + Send + Sync>;

/// State shared between the handle and the device callbacks.
pub(crate) struct Shared {
    pub playback: Playback,
    pub muted: AtomicBool,
    pub sink: ReportSink,
}

impl Shared {
    /// Collects microphone samples into chunks for the sink.
    pub fn capture(&self, pending: &mut Vec<i16>, samples: impl Iterator<Item = f32>) {
        if self.muted.load(Ordering::Relaxed) {
            pending.clear();
            return;
        }
        for sample in samples {
            pending.push((sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
            if pending.len() >= CAPTURE_CHUNK {
                (self.sink)(AudioReport::Captured(std::mem::take(pending)));
            }
        }
    }
}

/// The open devices. Dropping the handle stops them.
pub struct AudioIo {
    shared: Arc<Shared>,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl AudioIo {
    /// Open the default input and output devices.
    pub fn open(sink: ReportSink) -> Result<Self> {
        let shared = Arc::new(Shared {
            playback: Playback::new(sink.clone()),
            muted: AtomicBool::new(false),
            sink,
        });
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        let thread_shared = shared.clone();
        // Device handles are not `Send` on every platform: they live and die
        // on this thread.
        let thread = std::thread::Builder::new()
            .name("voice-audio".into())
            .spawn(move || {
                #[cfg(target_os = "macos")]
                let devices = macos::Devices::start(thread_shared);
                #[cfg(not(target_os = "macos"))]
                let devices = cpal_backend::Devices::start(thread_shared);
                match devices {
                    Ok(devices) => {
                        let _ = ready_tx.send(Ok(()));
                        // Wait for the handle to drop (or the sender to go).
                        let _ = stop_rx.recv();
                        drop(devices);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })?;
        ready_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("The audio thread ended before starting"))??;
        Ok(Self {
            shared,
            stop: Some(stop_tx),
            thread: Some(thread),
        })
    }

    pub fn play(&self, samples: &[i16]) {
        self.shared.playback.push(samples);
    }

    pub fn clear_playback(&self) {
        self.shared.playback.clear();
    }

    pub fn played_samples(&self) -> u64 {
        self.shared.playback.played()
    }

    pub fn set_muted(&self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Relaxed);
    }
}

impl Drop for AudioIo {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
