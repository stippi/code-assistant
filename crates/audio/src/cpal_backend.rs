//! Platforms without system echo cancellation: plain `cpal` streams.
//!
//! Without echo cancellation the microphone would hear the model, so it is
//! gated while audio plays and for a short tail after (half duplex).

use crate::resample::Resampler;
use crate::{AudioReport, SAMPLE_RATE, Shared};
use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long after playback the microphone stays gated (room echo).
const ECHO_TAIL: Duration = Duration::from_millis(300);

pub struct Devices {
    _input: Stream,
    _output: Stream,
}

impl Devices {
    pub fn start(shared: Arc<Shared>) -> Result<Self> {
        let host = cpal::default_host();
        let input_device = host.default_input_device().context("No microphone found")?;
        let output_device = host.default_output_device().context("No speakers found")?;
        let input = build_input(&input_device, shared.clone())?;
        let output = build_output(&output_device, shared)?;
        input.play()?;
        output.play()?;
        Ok(Self {
            _input: input,
            _output: output,
        })
    }
}

fn report_error(shared: &Arc<Shared>) -> impl FnMut(cpal::StreamError) + Send + 'static {
    let shared = shared.clone();
    move |e| (shared.sink)(AudioReport::Failed(e.to_string()))
}

fn build_input(device: &cpal::Device, shared: Arc<Shared>) -> Result<Stream> {
    let supported = device.default_input_config()?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let channels = config.channels as usize;
    let mut resampler = Resampler::new(config.sample_rate.0, SAMPLE_RATE);
    let mut mono = Vec::new();
    let mut resampled = Vec::new();
    let mut pending = Vec::new();
    let mut gated_until: Option<Instant> = None;
    let errors = report_error(&shared);

    let mut on_samples = move |frames: &mut dyn Iterator<Item = f32>| {
        mono.clear();
        let mut frame_sum = 0.0;
        for (i, sample) in frames.enumerate() {
            frame_sum += sample;
            if (i + 1) % channels == 0 {
                mono.push(frame_sum / channels as f32);
                frame_sum = 0.0;
            }
        }
        if shared.playback.is_playing() {
            gated_until = Some(Instant::now() + ECHO_TAIL);
        }
        if gated_until.is_some_and(|until| Instant::now() < until) {
            pending.clear();
            return;
        }
        resampled.clear();
        resampler.process(&mono, &mut resampled);
        shared.capture(&mut pending, resampled.iter().copied());
    };

    let stream = match format {
        SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _| on_samples(&mut data.iter().copied()),
            errors,
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _| {
                on_samples(&mut data.iter().map(|s| *s as f32 / i16::MAX as f32))
            },
            errors,
            None,
        )?,
        other => return Err(anyhow!("Unsupported microphone sample format {other:?}")),
    };
    Ok(stream)
}

fn build_output(device: &cpal::Device, shared: Arc<Shared>) -> Result<Stream> {
    let supported = device.default_output_config()?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let channels = config.channels as usize;
    let device_rate = config.sample_rate.0;
    let mut resampler = Resampler::new(SAMPLE_RATE, device_rate);
    let mut source = Vec::new();
    let mut converted: Vec<f32> = Vec::new();
    let errors = report_error(&shared);

    // Fill `frames` device frames (at the device rate) from the 24 kHz queue.
    let mut fill = move |frames: usize| -> Vec<f32> {
        while converted.len() < frames {
            let need = ((frames - converted.len()) as u64 * SAMPLE_RATE as u64 / device_rate as u64)
                .max(1) as usize;
            source.resize(need, 0.0);
            shared.playback.render(&mut source);
            resampler.process(&source, &mut converted);
        }
        converted.drain(..frames).collect()
    };

    let stream = match format {
        SampleFormat::F32 => device.build_output_stream(
            &config,
            move |data: &mut [f32], _| {
                let mono = fill(data.len() / channels);
                for (frame, sample) in data.chunks_mut(channels).zip(mono) {
                    frame.fill(sample);
                }
            },
            errors,
            None,
        )?,
        SampleFormat::I16 => device.build_output_stream(
            &config,
            move |data: &mut [i16], _| {
                let mono = fill(data.len() / channels);
                for (frame, sample) in data.chunks_mut(channels).zip(mono) {
                    frame.fill((sample * i16::MAX as f32) as i16);
                }
            },
            errors,
            None,
        )?,
        other => return Err(anyhow!("Unsupported speaker sample format {other:?}")),
    };
    Ok(stream)
}
