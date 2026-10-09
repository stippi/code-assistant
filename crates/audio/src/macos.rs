//! macOS: one voice-processing I/O unit for microphone and speakers.
//!
//! The unit cancels the echo of what it plays from what it records, so the
//! model's voice does not reach the realtime server as user speech. It also
//! converts between the device formats and the 24 kHz mono stream we ask for.

use crate::{SAMPLE_RATE, Shared};
use anyhow::{Context, Result};
use coreaudio::audio_unit::audio_format::LinearPcmFlags;
use coreaudio::audio_unit::render_callback::{self, data};
use coreaudio::audio_unit::{AudioUnit, Element, IOType, SampleFormat, Scope, StreamFormat};
use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject};
use std::sync::Arc;
use tracing::debug;

// AudioUnitProperties.h
const PROPERTY_STREAM_FORMAT: u32 = 8;
const PROPERTY_ENABLE_IO: u32 = 2003;
const PROPERTY_OTHER_AUDIO_DUCKING: u32 = 2108;
const DUCKING_LEVEL_MIN: u32 = 10;

/// `AUVoiceIOOtherAudioDuckingConfiguration` (macOS 14+).
#[repr(C)]
struct DuckingConfiguration {
    enable_advanced_ducking: u8,
    ducking_level: u32,
}

type Args = render_callback::Args<data::NonInterleaved<f32>>;

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {
    static AVMediaTypeAudio: *const AnyObject;
}

// AVAuthorizationStatus
const AUTHORIZATION_RESTRICTED: isize = 1;
const AUTHORIZATION_DENIED: isize = 2;

/// A microphone the user denied delivers silence, not an error. Check up
/// front; an undecided permission lets the system ask on first capture.
fn check_microphone_permission() -> Result<()> {
    let Some(device) = AnyClass::get(c"AVCaptureDevice") else {
        return Ok(());
    };
    // SAFETY: a class method taking an AVMediaType (an NSString constant
    // of the linked framework) and returning an NSInteger.
    let status: isize =
        unsafe { msg_send![device, authorizationStatusForMediaType: AVMediaTypeAudio] };
    match status {
        AUTHORIZATION_DENIED | AUTHORIZATION_RESTRICTED => anyhow::bail!(
            "Microphone access is denied; allow it in System Settings → Privacy & Security → \
             Microphone"
        ),
        _ => Ok(()),
    }
}

pub struct Devices {
    unit: AudioUnit,
}

impl Devices {
    pub fn start(shared: Arc<Shared>) -> Result<Self> {
        check_microphone_permission()?;
        let mut unit = AudioUnit::new_uninitialized(IOType::VoiceProcessingIO)
            .context("No voice-processing audio unit")?;
        let enable: u32 = 1;
        unit.set_property(
            PROPERTY_ENABLE_IO,
            Scope::Input,
            Element::Input,
            Some(&enable),
        )
        .context("Failed to enable the microphone")?;
        unit.set_property(
            PROPERTY_ENABLE_IO,
            Scope::Output,
            Element::Output,
            Some(&enable),
        )
        .context("Failed to enable the speakers")?;

        let format = StreamFormat {
            sample_rate: SAMPLE_RATE as f64,
            sample_format: SampleFormat::F32,
            flags: LinearPcmFlags::IS_FLOAT
                | LinearPcmFlags::IS_PACKED
                | LinearPcmFlags::IS_NON_INTERLEAVED,
            channels: 1,
        }
        .to_asbd();
        // What the microphone side hands us, and what we hand the speakers.
        unit.set_property(
            PROPERTY_STREAM_FORMAT,
            Scope::Output,
            Element::Input,
            Some(&format),
        )
        .context("Failed to set the capture format")?;
        unit.set_property(
            PROPERTY_STREAM_FORMAT,
            Scope::Input,
            Element::Output,
            Some(&format),
        )
        .context("Failed to set the playback format")?;

        // Voice processing ducks other apps' audio hard by default; keep the
        // user's music audible. Not available before macOS 14.
        let ducking = DuckingConfiguration {
            enable_advanced_ducking: 1,
            ducking_level: DUCKING_LEVEL_MIN,
        };
        if let Err(e) = unit.set_property(
            PROPERTY_OTHER_AUDIO_DUCKING,
            Scope::Global,
            Element::Output,
            Some(&ducking),
        ) {
            debug!("Ducking configuration not applied: {e}");
        }

        unit.initialize()
            .context("Failed to initialize the voice-processing unit")?;

        let capture = shared.clone();
        let mut pending = Vec::with_capacity(crate::CAPTURE_CHUNK);
        unit.set_input_callback(move |args: Args| {
            if let Some(channel) = args.data.channels().next() {
                capture.capture(&mut pending, channel.iter().copied());
            }
            Ok(())
        })
        .context("Failed to install the capture callback")?;

        let render = shared;
        unit.set_render_callback(move |mut args: Args| {
            if let Some(channel) = args.data.channels_mut().next() {
                render.playback.render(channel);
            }
            Ok(())
        })
        .context("Failed to install the playback callback")?;

        unit.start().context("Failed to start the audio devices")?;
        Ok(Self { unit })
    }
}

impl Drop for Devices {
    fn drop(&mut self) {
        let _ = self.unit.stop();
    }
}
