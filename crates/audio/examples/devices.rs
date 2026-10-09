//! Open the voice devices and report what they deliver.
//!
//! `cargo run -p audio --example devices` listens for two seconds without
//! playing anything; `-- --tone` also plays a one-second tone and reports
//! when the speakers drained (`-- --silence` the same with silent samples).

use audio::{AudioIo, AudioReport, SAMPLE_RATE};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let tone = std::env::args().any(|a| a == "--tone");
    let silence = std::env::args().any(|a| a == "--silence");
    let chunks = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(Mutex::new(0i16));
    let drained = Arc::new(AtomicUsize::new(0));
    let (c, p, d) = (chunks.clone(), peak.clone(), drained.clone());
    let io = AudioIo::open(Arc::new(move |report| match report {
        AudioReport::Captured(samples) => {
            c.fetch_add(1, Ordering::Relaxed);
            let max = samples
                .iter()
                .map(|s| s.saturating_abs())
                .max()
                .unwrap_or(0);
            let mut peak = p.lock().unwrap();
            *peak = (*peak).max(max);
        }
        AudioReport::Drained => {
            d.fetch_add(1, Ordering::Relaxed);
        }
        AudioReport::Failed(e) => eprintln!("device failed: {e}"),
    }))?;
    if silence {
        io.play(&vec![0; SAMPLE_RATE as usize]);
    }
    if tone {
        let samples: Vec<i16> = (0..SAMPLE_RATE)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                ((t * 440.0 * std::f32::consts::TAU).sin() * 3000.0) as i16
            })
            .collect();
        io.play(&samples);
    }
    std::thread::sleep(Duration::from_secs(2));
    println!(
        "captured chunks: {} (expected ~40), peak level: {}, played samples: {}, drained: {}",
        chunks.load(Ordering::Relaxed),
        peak.lock().unwrap(),
        io.played_samples(),
        drained.load(Ordering::Relaxed)
    );
    Ok(())
}
