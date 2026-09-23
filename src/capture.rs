//! Microphone capture: the one platform boundary in the plugin.
//!
//! The device is asked for 16 kHz mono directly, so there is no resampling.
//! ALSA's plug layer converts, which covers Linux; a device that cannot offer
//! the rate is reported rather than silently recorded at the wrong one.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::audio::{Decision, SilenceConfig, SilenceDetector, TARGET_RATE, to_mono_i16};
use crate::{Error, Result};

/// Bounded so a stalled consumer drops audio rather than growing without limit.
const QUEUE_FRAMES: usize = 256;

pub struct Recording {
    /// 16 kHz mono.
    pub samples: Vec<i16>,
    pub stopped_by: Decision,
    /// Whether loudness ever held long enough to count as speech.
    pub heard_speech: bool,
    /// RMS of the loudest 20 ms window, in `i16` units.
    pub loudest: f64,
}

impl Recording {
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.samples.len() as f64 / TARGET_RATE as f64)
    }
}

pub fn default_input() -> Result<cpal::Device> {
    cpal::default_host()
        .default_input_device()
        .ok_or(Error::NoInputDevice)
}

/// The format to open the device in: 16 kHz mono, `I16` if offered, else `F32`.
fn target_format(device: &cpal::Device) -> Result<cpal::SampleFormat> {
    let configs = device
        .supported_input_configs()
        .map_err(|e| Error::Audio(e.to_string()))?;
    let mut formats: Vec<cpal::SampleFormat> = configs
        .filter(|c| {
            c.min_sample_rate() <= TARGET_RATE
                && TARGET_RATE <= c.max_sample_rate()
                && c.channels() == 1
        })
        .map(|c| c.sample_format())
        .collect();
    formats.sort_by_key(|f| !matches!(f, cpal::SampleFormat::I16));
    formats
        .into_iter()
        .find(|f| matches!(f, cpal::SampleFormat::I16 | cpal::SampleFormat::F32))
        .ok_or_else(|| {
            Error::Audio(format!(
                "no {TARGET_RATE} Hz mono input; resampling is not implemented"
            ))
        })
}

/// Record until the detector or `stop` says to finish.
pub fn record(silence: SilenceConfig, stop: Arc<AtomicBool>) -> Result<Recording> {
    let device = default_input()?;
    let format = target_format(&device)?;
    let config = cpal::StreamConfig {
        channels: 1,
        sample_rate: TARGET_RATE,
        buffer_size: cpal::BufferSize::Default,
    };

    let (tx, rx) = mpsc::sync_channel::<Vec<i16>>(QUEUE_FRAMES);
    let on_error = |err| tracing::warn!(%err, "input stream error");

    let stream = match format {
        cpal::SampleFormat::I16 => device.build_input_stream(
            config,
            move |data: &[i16], _: &_| {
                let _ = tx.try_send(data.to_vec());
            },
            on_error,
            None,
        ),
        _ => device.build_input_stream(
            config,
            move |data: &[f32], _: &_| {
                let _ = tx.try_send(to_mono_i16(data, 1));
            },
            on_error,
            None,
        ),
    }
    .map_err(|e| Error::Audio(e.to_string()))?;

    stream.play().map_err(|e| Error::Audio(e.to_string()))?;
    tracing::info!(rate = TARGET_RATE, ?format, "recording");

    let mut detector = SilenceDetector::new(silence);
    let mut samples = Vec::new();
    let mut stopped_by = Decision::Continue;

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(frame) => {
                let decision = detector.push(&frame);
                samples.extend_from_slice(&frame);
                if decision != Decision::Continue {
                    stopped_by = decision;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    drop(stream);
    Ok(Recording {
        samples,
        stopped_by,
        heard_speech: detector.heard_speech(),
        loudest: detector.loudest(),
    })
}

/// One line describing the input, for `doctor`.
pub fn describe_input() -> Result<String> {
    let device = default_input()?;
    let name = device
        .description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| "<unnamed>".into());
    let format = match target_format(&device) {
        Ok(format) => format!("{TARGET_RATE} Hz mono {format:?}"),
        Err(err) => err.to_string(),
    };
    Ok(format!("{name} - will open at {format}"))
}

/// Write 16 kHz mono samples as a WAV file.
pub fn write_wav(path: &std::path::Path, samples: &[i16]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer =
        hound::WavWriter::create(path, spec).map_err(|e| Error::Audio(e.to_string()))?;
    for &sample in samples {
        writer
            .write_sample(sample)
            .map_err(|e| Error::Audio(e.to_string()))?;
    }
    writer.finalize().map_err(|e| Error::Audio(e.to_string()))
}
